# Catalog migrations

**Status:** Draft
**Date:** 2026-09-30
**Companions:** [Architecture overview](overview.md), [Implementation plan](../plan.md)

A catalog is one SQLite file. Two kinds of change could make an old file fail to open, and they are handled separately.

- **Schema version** is ours. It is an integer in `catalog_meta`. The library applies ordered SQL until the stored integer matches the binary.
- **Storage format** belongs to SQLite. It has been stable and backward compatible since 2004, so in practice this is a question of which SQLite features a schema uses, not of which library version wrote the file.

BPM-Lite import is the open question in the PRD. It is not a schema migration.

## Schema version

`catalog_meta` holds `schema_version` as a decimal integer in the text `value` column. Version `0` means the file has been created and no migration has run. A missing key on a file that already has tables is corrupt. The library refuses to open it and does not guess a version.

The newest migration embedded in the binary is the version that binary understands. A file at version 2 opened by a binary whose newest migration is 5 runs 3, then 4, then 5. A file newer than the binary is refused. The message names both versions. The file is not modified.

## The runner

SQL files live in the repository and are compiled into the binary. Names are `V001__init.sql`, `V002__short_name.sql`, and so on. The numbers start at 1, increase by one, and have no gaps. The process checks that sequence at startup.

`bpm init` creates an empty SQLite file, sets `PRAGMA application_id` to BPM's value so a non-BPM file can be refused on open, switches it to WAL mode, and runs this same runner from version 0. There is no second copy of the schema for new catalogs.

A migration runs only from a writer, and only inside a write transaction started with `BEGIN IMMEDIATE`, which is the catalog write lock in the architecture overview. `bpm serve` opens the catalog read-only and does not migrate. If the file is older than the binary, serve exits and names both versions. The operator then runs a writing command, which migrates, and starts serve again.

Each file runs in one transaction. The last statement in that transaction sets `schema_version` to the file's number. SQLite DDL is transactional, so a statement that fails rolls back every table, index, and view change in that file along with the version. The stored version stays where it was, and the next open tries that file again. A shipped migration is not edited. A change is a new file with the next number.

## What a migration contains

A migration creates tables, adds columns, adds indexes, or copies rows into a rebuilt table. The first migration creates `catalog_meta` and the node, file, and metadata tables in the architecture overview.

Fingerprint schemes and digest algorithms are values in existing columns. A new scheme or a new algorithm does not rewrite old rows and does not need a migration. File ids are never reassigned.

Prefer an additive change. Rebuild a table when SQLite cannot express the change as `ALTER TABLE`. That copy is the whole migration, in the one transaction, and a test loads a catalog large enough to show the copy finishes under the lock.

Migrations stay within the logical types the architecture overview already allows for a later Postgres copy: UUID, text, timestamps, booleans, and ordinary foreign keys, written with the physical mapping in overview §5. Every new table is `STRICT`.

## Limits of SQLite's `ALTER TABLE`

These limits are why some migrations are a copy rather than an `ALTER`.

- `ALTER TABLE` supports `RENAME TO`, `RENAME COLUMN`, `ADD COLUMN`, and `DROP COLUMN`. Nothing else.
- `ADD COLUMN` cannot add a primary key or `UNIQUE` column. A `NOT NULL` column needs a non-null constant default. A column with `REFERENCES` must default to null.
- `DROP COLUMN` fails if the column is part of a primary key, a `UNIQUE` constraint, an index, a foreign key, a `CHECK`, or a view such as `entities`. Drop the index or view first, or rebuild.
- Adding or dropping a constraint, changing a column's type, or changing a primary key is a rebuild: create the new table, copy, drop the old one, rename, then recreate its indexes and any view that reads it.
- A rebuild of a table that other tables reference must turn `foreign_keys` off for the connection before `BEGIN`, because the pragma has no effect inside a transaction. The migration runs `PRAGMA foreign_key_check` before it commits, and fails if any row is reported. The runner turns `foreign_keys` back on afterwards. This is SQLite's documented twelve-step procedure. **The runner cannot do this yet.** It always opens the transaction before running a file, and never changes `foreign_keys`. See "Before the next migration" in the [implementation plan](../plan.md).
- A migration takes the one writer. Readers keep reading the last committed state while it runs. Other writers see the catalog as busy, under the same lock rules as any other write.

## Storage format

The `rusqlite` crate is pinned, with the SQLite engine bundled. A newer SQLite reads every file an older one wrote. An older SQLite reads a newer file unless the schema uses a feature it lacks. The catalog uses `STRICT` tables, so any engine older than 3.37 (2021) refuses it. That includes some system `sqlite3` shells an operator might point at the file. Such a refusal is reported as a storage-format problem. It is not described as a schema version.

Bumping the crate is its own change. Before it merges, a catalog written by the previous pin is opened with the new pin, migrated if the schema version also moved, and read back. A migration does not start using a SQLite feature newer than the oldest engine BPM supports without saying so in the migration and in the release notes.

The operator copies the catalog file while no `bpm` process has it open, then upgrades the binary. Read-only commands, including `bpm serve`, may leave an empty `-wal` file and a `-shm` file beside the catalog. Both are normal, and the next writer removes them when it exits. A `-wal` file that is not empty after every `bpm` process has exited means a writer did not exit cleanly. SQLite replays it on the next open, so it is not corruption, but a copy of the main file alone would miss those commits. Copy the `-wal` file too, or run any writing `bpm` command once so the log is checkpointed.

## Tests

- A fresh `bpm init` ends at the newest embedded version.
- A saved catalog at an older version migrates to the newest one, and a smoke read returns the rows it had.
- A catalog whose `schema_version` is newer than the binary is refused, and the file's modification time is unchanged.
- A migration whose SQL fails leaves the stored version unchanged.
- A read-only open does not change `schema_version`.
- A rebuild migration on a table that other tables reference passes `PRAGMA foreign_key_check`.

## Govern

PRD §5 is unstable. If an operations database is built, it uses this same runner and its own `schema_version`. A catalog and the operations database can move on different days.
