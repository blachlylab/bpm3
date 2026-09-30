# Catalog migrations

**Status:** Draft
**Date:** 2026-09-30
**Companions:** [Architecture overview](overview.md), [Implementation plan](../plan.md)

A catalog is one DuckDB file. Two kinds of change can make an old file fail to open, and they are handled separately.

- **Schema version** is ours. It is an integer in `catalog_meta`. The library applies ordered SQL until the stored integer matches the binary.
- **Storage format** belongs to the DuckDB library linked into the binary. Replacing that library is a dependency change, not a schema migration.

BPM-Lite import is the open question in the PRD. It is not a schema migration.

## Schema version

`catalog_meta` holds `schema_version` as a decimal integer in the text `value` column. Version `0` means the file has been created and no migration has run. A missing key on a file that already has tables is corrupt. The library refuses to open it and does not guess a version.

The newest migration embedded in the binary is the version that binary understands. A file at version 2 opened by a binary whose newest migration is 5 runs 3, then 4, then 5. A file newer than the binary is refused. The message names both versions. The file is not modified.

## The runner

SQL files live in the repository and are compiled into the binary. Names are `V001__init.sql`, `V002__short_name.sql`, and so on. The numbers start at 1, increase by one, and have no gaps. The process checks that sequence at startup.

`bpm init` creates an empty DuckDB file and runs this same runner from version 0. There is no second copy of the schema for new catalogs.

A migration runs only from a writer, and only while that process holds the advisory lock from the architecture overview. `bpm serve` opens the catalog read-only and does not migrate. If the file is older than the binary, serve exits and names both versions. The operator then runs a writing command, which migrates, and starts serve again.

Each file runs in one transaction. The last statement in that transaction sets `schema_version` to the file's number. A statement that fails rolls the transaction back. The stored version stays where it was, and the next open tries that file again. A shipped migration is not edited. A change is a new file with the next number.

## What a migration contains

A migration creates tables, adds columns, adds indexes, or copies rows into a rebuilt table. The first migration creates `catalog_meta` and the node, file, and metadata tables in the architecture overview.

Fingerprint schemes and digest algorithms are values in existing columns. A new scheme or a new algorithm does not rewrite old rows and does not need a migration. File ids are never reassigned.

Prefer an additive change. Rebuild a table when DuckDB cannot express the change as `ALTER TABLE`. That copy is the whole migration, in the one transaction, and a test loads a catalog large enough to show the copy finishes under the lock.

Migrations stay within the types the architecture overview already allows for a later Postgres copy: UUID, text, timestamps, booleans, and ordinary foreign keys. They do not use DuckDB-only types.

## Limits of DuckDB's `ALTER TABLE`

These limits are why some migrations are a copy rather than an `ALTER`.

- `ALTER TABLE` is transactional. A failed statement rolls back with the version update.
- Adding and dropping constraints is not supported. Primary keys, unique keys, and foreign keys are declared in `CREATE TABLE`. Changing them means creating a new table, copying, and swapping the name.
- `ON DELETE CASCADE` is not available. Parent foreign keys stay `ON DELETE RESTRICT`, which is the product rule anyway.
- Dropping a column that an index depends on requires dropping the index first.
- Changing a column type can fail because of a value that used to be in the column, even after that value was deleted. The workaround is a new table and a copy.
- A migration takes the one writer. Readers wait or see the catalog as busy, under the same lock rules as any other write.

## Storage format

The `duckdb` crate version is pinned. A newer crate can refuse a file written by the pinned one, and an older crate can refuse a file the newer one has rewritten. That failure is reported as a storage-format mismatch. It is not described as a schema version.

Bumping the crate is its own change. Before it merges, a catalog written by the previous pin is opened with the new pin, migrated if the schema version also moved, and read back. The operator copies the catalog file while no `bpm` process has it open, then upgrades the binary. A leftover write-ahead file means a writer did not exit cleanly. The upgrade waits until that file is gone.

## Tests

- A fresh `bpm init` ends at the newest embedded version.
- A saved catalog at an older version migrates to the newest one, and a smoke read returns the rows it had.
- A catalog whose `schema_version` is newer than the binary is refused, and the file's modification time is unchanged.
- A migration whose SQL fails leaves the stored version unchanged.
- A read-only open does not change `schema_version`.

## Govern

PRD §5 is unstable. If an operations database is built, it uses this same runner and its own `schema_version`. A catalog and the operations database can move on different days.
