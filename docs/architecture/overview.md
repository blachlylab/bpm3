# BPM Architecture Overview

**Status:** Draft 0.1
**Date:** 2026-09-30
**Companion:** [Product requirements](../product/prd.md)

This document records the implementation choices behind the product requirements. Behavior lives in the PRD. Where a mechanism is visible to an operator, the PRD is the contract and this document says how it is met. Sections the PRD marks deferred or unstable are marked the same way here.

The Cargo package is still named `bpm3`. The binary target is `bpm` (`cargo build` writes `target/debug/bpm`). Renaming the package is a later cleanup.

## 1. Stack

| Layer | Choice | Role |
| --- | --- | --- |
| Language | Rust, edition 2024 | One static binary. Memory safety for a program that handles human-subjects metadata. |
| CLI | Clap 4, derive API | The `bpm` vocabulary in the PRD. |
| Catalog store | SQLite, embedded, via the `rusqlite` crate with the bundled engine | One file per catalog. Declared foreign keys, indexed lookups over millions of file and metadata rows, and concurrent readers beside one writer across processes. No separate database process in Core. The choice is [ADR 0001](../adr/0001-sqlite-catalog-store.md). |
| HTTP | Axum | Local `bpm serve`. `bpmd` is PRD §5, which is unstable. |
| HTML | Askama templates, HTMX attributes in those templates | Server-rendered pages. Partials swap into the existing page. |
| Front-end assets | HTMX, the compiled stylesheet (Tailwind CSS and DaisyUI), and any other JS or CSS | Vendored in the repository under `assets/vendor/`. A build embeds those files and does not download them. Regenerating the stylesheet is optional and its output is committed back. |

JS, CSS, and the HTMX runtime are vendored into the tree. The `bpm` binary embeds the copies under `assets/vendor/`.

Identity providers and the policy-review client were sketched for Govern. PRD §5 is **UNSTABLE / TBD**, so they are not stack decisions yet.

Core dependencies stay on the library side of a trait so that Clap types and Axum types never appear in catalog code.

## 2. Two shapes, one library

```
┌────────────┐     in process      ┌──────────────────────────┐
│  bpm CLI   │────────────────────▶│                          │
└────────────┘                     │  bpm-core                │
┌────────────┐     in process      │  model, catalog trait,   │
│ bpm serve  │────────────────────▶│  query (v1 read-only)    │
│  Axum+HTMX │                     │                          │
└────────────┘                     └────────────┬─────────────┘
                                                │
                                                ▼
                                      catalog file (SQLite)

Later:

┌────────────┐  HTTP or gRPC, trait ┌─────────────┐     ┌──────────────────┐
│  bpm CLI   │─────────────────────▶│    bpmd     │────▶│ catalog files    │
└────────────┘                      │  Axum+HTMX  │     │ + operations DB  │
                                    │  auth, audit│     └──────────────────┘
                                    │  policy     │
                                    └─────────────┘
```

The top half is Core. The CLI calls the catalog trait in-process. `bpm serve` in v1 calls the read-only query side of that trait in-process. The lower half is `bpmd` and is **UNSTABLE / TBD**, matching PRD §5. If it is built, it is a second binary in this repository linking the same library, not a second data model.

The interface between `bpm` and `bpmd` is that trait over a network. The transport is not chosen. It may be HTTP. It may instead be gRPC, or something similar, which might be far more efficient for large binary data. The browser UI is separate: the sketch still serves it with Axum and HTMX, and the CLI does not have to use that HTTP stack to call the trait.

### Crate and module boundary

The first code can live in one Cargo package. The boundary that matters is the module boundary, so a later split into crates is a move rather than a rewrite.

| Module | May depend on | Must not depend on |
| --- | --- | --- |
| `model` | std, serde | clap, axum, rusqlite |
| `catalog` | `model`, rusqlite behind the trait implementation | clap, axum |
| `ingest` | `model`, filesystem | clap, axum, SQL strings from the CLI |
| `query` | `model`, `catalog` trait | clap, axum |
| `cli` | clap, the traits | axum, rusqlite (it sees the trait, not the engine) |
| `web` | axum, askama, the traits, vendored assets | clap, rusqlite |

`ingest` implements both `bpm ingest` and `bpm scan`, and v1 calls it from the CLI only. The read-only web UI does not ingest or scan. HTML handlers format the same query structs the CLI prints. They do not grow a second query language. `manifest` and `import` are PRD §4.9 and §4.10, which are unstable, so they are not modules in the settled layout.

The SQLite implementation is the only module that embeds SQL for catalog mutations. Query methods on the trait (`entities_under`, `files_under`, `summary`) are the unstable sketch in §7 and PRD §4.8. `impact` and `lineage` wait for PRD §4.7.

## 3. The catalog trait

The trait is the reuse seam. Names here are conceptual; Rust signatures can follow the code style of the repo when the trait is written.

| Operation group | What it covers |
| --- | --- |
| Catalog | Open, migrate to latest, catalog id, label, integrity check, repair |
| Entities | Create, rename, reparent, delete, cascade delete |
| Metadata | Set, unset, list, on entities and on files |
| Observations | Apply one ingest batch: new locations, sizes, mtimes, fingerprints. Consult existing rows only for a duplicate |
| Digests | `bpm scan` stores BLAKE3 when it reads bytes, or the backend's checksum when that backend supplies one and the body is not downloaded. Acknowledge starts a new generation and clears every current digest for that file |
| Links | Link, unlink, role. No primary flag |
| Derivations | **Deferred (PRD §4.7).** Derived-from edges and the walks |
| Query | **Unstable (PRD §4.8).** Entity, file, and summary queries |
| Manifest | **Unstable (PRD §4.10).** Canonical body, content id, snapshot id |
| History | **Deferred (PRD §4.7).** Command notebook |
| SQL | Read-only by default. Implemented for the local SQLite catalog only. `--write` does not record history until §4.7 |

A `Catalog` value in Core is `LocalSqlite`. The CLI resolves the file by the PRD order: `--catalog`, then the `BPM_CATALOG` environment variable, then `~/.bpm/default.db`. A remote implementation is part of Govern and is **UNSTABLE / TBD** with PRD §5. Its transport is not chosen; see §2.

Errors are typed enough that the CLI and the web UI can share messages: illegal parent, sibling name taken, catalog busy, path outside the ingest root. The UI does not parse English error strings.

### Remote trait constraints

**UNSTABLE / TBD (PRD §5).**

A remote client would implement the same groups except SQL, on whichever transport §2 settles. The server rejects a SQL call so an arbitrary statement cannot bypass a future policy filter.

Remote ingest and scan still run against storage the client can see, or against storage the server can see. The first Govern deployment, if it is built, assumes `bpmd` and the data share a filesystem namespace, which matches a core facility or a lab server. Object-store locations are the later Core milestone in PRD §3, and this sketch does not define them. A laptop pushing observations of paths the server cannot read is a later mode: the client would upload the scan batch (paths, sizes, mtimes, fingerprints, digests) and the server would apply it through the same batch operation. The trait's batch operation is what makes that mode possible without a second ingestion path. It is not part of the Core release to expose that batch endpoint publicly.

## 4. Local process model

- One `bpm` process opens one catalog per invocation.
- `bpm serve` in v1 holds a read-only connection. It does not take the write lock and it cannot mutate the catalog.
- The catalog runs in SQLite's write-ahead-log mode (`journal_mode=WAL`). Any number of processes can read while one process writes, and each reader sees the last committed state. `bpm serve` keeps reading while `bpm ingest` commits batches in another process.
- The catalog write lock is SQLite's own. A write transaction starts with `BEGIN IMMEDIATE`, which takes the write lock up front, and holds it only for the duration of that transaction. There is no separate `<catalog>.lock` file. Readers, including `bpm serve`, do not take the write lock. Walking a directory and hashing bytes do not take it.
- A writer that cannot acquire the lock within a few seconds (`busy_timeout`) exits with "catalog is busy", naming the catalog file. It does not queue silently.
- Readers keep their transactions short, one per CLI command or per HTTP request. A long-lived read transaction stops the WAL from being checkpointed back into the main file, and the WAL grows until it ends.
- WAL mode requires every process using the catalog to be on the same host, because readers and the writer coordinate through shared memory (the `-shm` file). The catalog file must live on a local filesystem, not NFS or SMB. The files the catalog describes may live anywhere.
- Ingest and scan hash a batch of files with the catalog lock released, then take the lock to commit that batch, then release it and hash the next batch. The batch is 1,000 files. That count is internal. A crash keeps every batch already committed. The next run continues.
- A long run's liveness is a separate file, `<catalog>.run-<id>.lock`, held with an exclusive flock for the whole command. It is not the catalog write lock, so other writers proceed between batches. The kernel releases the flock if the process dies. The next open that finds a run still marked `running`, and that can acquire this flock, marks the run `incomplete` and removes the file.
- Read-write actions from the web UI are Core v2 and are TBD. They are not in v1.

Catalog files and `~/.bpm` are created with mode `0600` for files and `0700` for directories. Every command warns on stderr, and still runs, when `~/.bpm` grants any access to group or other users. SQLite creates the `-wal` and `-shm` files beside the catalog with the catalog file's permissions. The embedded engine does not add its own encryption. Disk encryption is the operator's, or the institution's, responsibility. The PRD's statement that anyone who can read the file can read the metadata is this choice.

### User config

`~/.bpm/context.toml` may record named catalogs. It has no `current` key, and a command never opens a catalog because the file names it. Resolution stays `--catalog`, then the `BPM_CATALOG` environment variable, then `~/.bpm/default.db`. Walk filters do not live in this file.

`~/.bpm/config.toml` holds global tool settings. Today that is the blacklist, described in §6. A command reads it for those settings. It does not select a catalog.

A remote catalog entry, and `bpm use`, belong to Govern and are **UNSTABLE / TBD** (PRD §5 and the `*` rows in PRD §4.13). The `bpm serve` token is `--token` or `BPM_TOKEN`. It is not written into either file. `context.toml` may look like this:

```toml
[catalogs.cll]
kind = "local"
path = "/Users/me/work/cll/bpm.db"

[catalogs.core]
kind = "remote"
url = "https://bpm.example.org"
catalog = "pathology"
```

A `~/.bpm/credentials/` directory for Govern refresh tokens is **UNSTABLE / TBD** with PRD §5. The v1 serve login, when a token is configured, is an `HttpOnly` session cookie on `bpm serve` itself.

## 5. Logical schema

Types below are the logical schema. SQLite is the physical store. UUID and UTC timestamps are the only non-primitive types. Every UUID the library assigns — catalog id, node id, file id, and the ids in later tables — is a UUIDv7, generated in the library rather than by the database.

Foreign keys are declared. Each node table's parent column references the table one step up, and every `file_id` column references `files.id`. The library also checks a parent in the same transaction, so an error names the illegal parent instead of reporting a constraint number. `(node_type, node_id)` on a link or a metadata row cannot be a foreign key, because it names one of six tables. The library checks that pair on write, and a node delete removes those rows in the same transaction. `bpm sql --write` can still insert a `(node_type, node_id)` pair whose node is missing. It cannot insert a missing parent id or `file_id`, because `bpm` turns foreign-key enforcement on for that connection too.

### Physical store

These rules are how the logical schema is written in SQLite. They stay inside the SQLite catalog implementation and its migrations.

| Logical type | SQLite column |
| --- | --- |
| UUID | `TEXT`, the canonical 36-character lowercase form. Readable in `bpm sql` |
| UTC timestamp | `TEXT`, ISO 8601 with a `Z` suffix and millisecond precision. Sorts as it reads |
| Boolean | `INTEGER` with `CHECK (x IN (0, 1))` |
| Integer, size in bytes | `INTEGER` (64-bit) |
| Text | `TEXT` |

- Every table is `STRICT`, so a value of the wrong type is rejected instead of being stored as written. A table whose primary key is not a single integer is also `WITHOUT ROWID`, so the primary key is the table's own B-tree and not a second index.
- Every connection `bpm` opens sets `foreign_keys=ON`, `busy_timeout`, and, on a writer, `journal_mode=WAL` and `synchronous=FULL`. Whether a new connection enforces foreign keys depends on how SQLite was built. The bundled engine enforces them by default and `bpm` sets the pragma anyway. A third-party SQLite shell often does not enforce them, and can write rows that break a declared key, the same way it can bypass any library check. The integrity check below is how `bpm` notices.
- `STRICT` needs SQLite 3.37 or later. An older `sqlite3` shell refuses to open the catalog. The bundled engine is far newer.
- `PRAGMA optimize` runs at the end of every ingest and scan run, so the planner has the statistics it needs to choose the `(key, value)` indexes. A busy catalog skips it and the run still succeeds.
- V002 adds two indexes the file indexer needs: `files (size_bytes, fingerprint_scheme, fingerprint)`, which ingest probes for every new path before deciding whether to hash it, and `file_digests (algorithm, digest)` for `bpm query files --digest`.

### Integrity check and repair

Before every command that walks or extends the entity tree, `bpm` runs `PRAGMA foreign_key_check` on the five node tables that have a parent. Any row reported fails the command with "catalog is inconsistent" and the advice to run `bpm repair`. The check covers only the tree because the tree is what every walk depends on, and because it is small. On 100,000 entities it takes about 25 ms. Checking the file tables too would mean reading every metadata row on every command. `sql`, `repair`, `reparent`, and `delete` skip the check, so an operator can inspect and fix a broken tree.

`bpm repair` runs the full check. That covers every declared foreign key, plus the `(node_type, node_id)` references on `entity_metadata` and `file_links`, which are not foreign keys. It lists the rows by key. `--apply` removes them in one `BEGIN IMMEDIATE` transaction:
- an entity whose parent is missing, with its subtree, through the same path as `delete --cascade`;
- metadata and link rows that name a missing entity;
- `file_*` rows that name a missing file;
- error rows that name a missing run.

The transaction commits only if a whole-catalog `PRAGMA foreign_key_check` and the reference checks then find nothing. File rows are never removed.

A tree walk that still meets a missing parent, for example in a catalog changed between the check and the read, reports the same inconsistency error. It does not panic. The operator procedure, including the gentler fixes to try before `--apply`, is the [repair guide](../guide/repair.md).

### catalog_meta

| Column | Notes |
| --- | --- |
| key | Primary key. Rows include `catalog_id`, `label`, `created_at`, `schema_version` |
| value | Text |

`catalog_id` is a UUID assigned at init and stable for the life of the file.

### Node tables

Six tables. Ids follow the UUIDv7 rule above. `created_at` and `updated_at` are UTC on every table. A parent column is `NOT NULL` and is a foreign key to the table one step up, `ON DELETE RESTRICT`. The library also refuses a delete while children or file links remain, because file links are not a foreign key.

| Table | Parent | Name |
| --- | --- | --- |
| `programs` | none | Required. Unique |
| `projects` | `program_id` → `programs.id` | Required. Unique among siblings |
| `cases` | `project_id` → `projects.id` | No name column |
| `samples` | `case_id` → `cases.id` | No name column |
| `raw_data` | `sample_id` → `samples.id` | No name column |
| `analyses` | `raw_data_id` → `raw_data.id` | No name column |

A Sample cannot name a Project: the column `samples.case_id` references `cases` only. Reparent updates that parent column. The foreign key checks the new parent. Rename updates `name` on a Program or Project and does not touch link rows. `--cascade` deletes in one transaction, leaves first, so `ON DELETE RESTRICT` never sees a parent whose children still exist.

A view `entities` is the `UNION ALL` of the six tables, with columns `node_type`, `id`, `parent_id`, and `name` (`name` is null below Project). A filter on `node_type` skips the other branches. The view is not a second store. Catalog reads and ad hoc SQL may use it. Writes go to the typed table. A UUID lookup without a type reads this view, which probes the six primary keys.

Descendant sets are computed by joining these tables along the parent columns. The chain is at most five steps. The schema does not store a closure. Add one only if the benchmark shows that gathering descendant ids, rather than reading link and file rows, is the slow step.

### entity_metadata

| Column | Notes |
| --- | --- |
| node_type | `program`, `project`, `case`, `sample`, `raw_data`, or `analysis` |
| node_id | UUID in the table `node_type` names. Not a foreign key |
| key | Text. Non-empty. No `:` or `/` |
| value | Text. Non-empty. No `:` or `/` |
| updated_at | UTC |

Primary key `(node_type, node_id, key)`. One value per key. An index on `(key, value)` supports the selectors in PRD §4.4. On insert and update the library checks that `node_id` exists in the named table. Deleting a node deletes its metadata rows in the same transaction.

Core does not give any key special behavior. `consent` and `embargo_until` are stored here like any other pair. Govern's reading of them is PRD §5, which is unstable. A later release may give some keys special meaning, including in Core, and may require some of them. That is not in this schema yet.

### file_metadata

| Column | Notes |
| --- | --- |
| file_id | Foreign key to `files.id` |
| key | Text. Non-empty. No `:` or `/` |
| value | Text. Non-empty. No `:` or `/` |
| updated_at | UTC |

Primary key `(file_id, key)`. An index on `(key, value)` supports the same selectors against files. File metadata is not stored on `entity_metadata`.

### files

| Column | Notes |
| --- | --- |
| id | UUID |
| size_bytes | Last observed size at a present location, denormalized for query |
| mtime | Last observed mtime, UTC |
| fingerprint | Hex, or null when the bytes were not read |
| fingerprint_scheme | Text, or null when `fingerprint` is null. The first schemes are `xxh3-128-full` and `xxh3-128-sample-v1`. Two fingerprints are compared only when the scheme strings are equal. A new scheme is a new string on newly written rows. Existing rows are not rewritten |
| created_at | First discovery |

The file id does not depend on the fingerprint. Links, locations, and digests stay valid if the scheme changes or the fingerprint is null. The current digests are rows, not columns, so a computed BLAKE3 and a backend checksum of another algorithm coexist.

### file_digests

| Column | Notes |
| --- | --- |
| file_id | |
| algorithm | `blake3` when BPM reads the bytes. `md5` when the operator requests it. A backend checksum uses the provider's name, such as `sha256` or `xxh3` |
| digest | Lowercase hex, without the algorithm prefix |
| source | `computed` or `backend` |
| generation | Integer, starts at 1 |
| current | Boolean. One current row per `(file_id, algorithm)` |
| observed_at | UTC |

The wire form is `algorithm:<hex>`, so a digest BPM computed is `blake3:<hex>`. The prefix is added at the edges, not stored twice.

Acknowledge clears `current` on every digest row for that file, then inserts the digests observed for the new bytes. Older rows stay in place. That is the generation history. There is no separate history table. An algorithm that was not observed again does not remain current.

### file_locations

| Column | Notes |
| --- | --- |
| file_id | |
| backend | `posix` in the first milestone. `s3` and other object stores are a later Core milestone (PRD §3) |
| uri | Absolute path |
| first_seen_at, last_seen_at | UTC |
| last_size, last_mtime | Observation at `last_seen_at` |
| presence | `present` or `missing` |
| stat_state | `unchanged`, `changed`, `unknown`. `changed` stays set until acknowledge |
| digest_state | `unverified`, `match`, `mismatch` |

Primary key `(backend, uri)`. A path belongs to one file. When a move is confirmed, the old uri is marked `missing` and stays until the operator deletes that location. A second present path with the same digest inserts a second location row for the same `file_id`.

`backend` is a column from the first migration so an object-store URI can be added without a new identity model. The first milestone writes `posix` only. Remote object stores, including S3, are a later Core milestone (PRD §3), not a Govern feature.

### file_links

| Column | Notes |
| --- | --- |
| file_id | Foreign key to `files.id` |
| node_type | Same six names as `entity_metadata` |
| node_id | UUID in the table `node_type` names. Not a foreign key |
| role | Text |

Primary key `(file_id, node_type, node_id)`. An index on `(node_type, node_id)` serves "files on this node" and cascade delete. `files` has no node column. Ingest writes `files` and `file_locations` only. Zero link rows is an unlinked file, which is normal after ingest. Two link rows attach one file id to two nodes. The library checks that `node_id` exists on insert. A delete with link rows still present fails. `--cascade` deletes the descendant nodes, their link rows, and their `entity_metadata` rows, and leaves the file rows. No primary-link column. That flag is undefined until the product gives it behavior.

### file_derivations

**Deferred (PRD §4.7).** The table below is the sketch. It is not part of the settled schema.

| Column | Notes |
| --- | --- |
| output_file_id, input_file_id | Primary key |
| recorded_at | UTC |

Cycles are rejected in the library when an edge is recorded, by walking ancestors of the input. The walk is bounded by the edge count of the two files' connected component.

### ingest_runs and scan_runs

Ingest and scan are different commands, so their run logs are different tables with the same columns: id, backend (empty when a scan covers every backend), root_uri (empty when a scan has no path), started_at, finished_at, status (`running`, `complete`, `incomplete`), files_seen, files_created (ingest only). Errors are rows of run id, uri, and message. A path that cannot be read is an error row. It does not make the run `incomplete`. The command reports it on stderr and exits non-zero after committing everything else.

A run left `running` by a crash is `incomplete` the next time a process opens the catalog and can take that run's liveness flock. The catalog lock is not the signal, because it is released between batches of a live run. Running the command again is the recovery. Individual file batches are already committed. Ingest does not mark missing paths. Scan does.

### manifests

**Unstable (PRD §4.10).** The table below is the sketch. It is not part of the settled schema.

| Column | Notes |
| --- | --- |
| id | UUID |
| content_id | Hex of the canonical content body |
| snapshot_id | Hex of the snapshot body |
| content_body | Canonical JSON |
| snapshot_body | Canonical JSON including locations and drift |
| selector | JSON of the filters that were asked for |
| created_at | UTC |

Unique on `content_id`. A request that canonicalizes to an existing content body returns the stored row. When the content matches and the newly computed snapshot differs, the command returns the stored `content_id` and prints the new snapshot hash, and leaves the stored snapshot in place. `--refresh-snapshot` updates `snapshot_body` and `snapshot_id` on that row and leaves `content_id` and `content_body` alone. That refresh is the only update a manifest row accepts. It records a storage move without minting a second scientific object.

A partial materialize writes its report next to the staged files. It does not add a column to this table and does not rewrite `content_body`.

### command_history

**Deferred (PRD §4.7).** The table below is the sketch. `bpm sql --write` does not append a row until this milestone exists.

| Column | Notes |
| --- | --- |
| id | Integer |
| at | UTC |
| os_user | The local user name. Not an authenticated identity |
| cwd | |
| catalog_id | |
| action | Argv for CLI, or a stable verb plus object ids for a UI action |

Append-only through the library. No update method is exposed.

### What is computed, not stored

Possible duplicates (same size, same fingerprint scheme and value, different file id, no shared current digest of the same algorithm) are a query over `files` and `file_digests`. Drift filters are a query over `file_locations`, using the operator states in §6. Descendant sets are a join along the node-table parent keys, not a stored closure. A Govern release would compute effective policy from the tree at decision time and copy it into the release record. That step is PRD §5, which is unstable. Core does not compute it.

## 6. Ingest, scan, fingerprint, and digest

### Fingerprint

The fingerprint is a fast candidate filter. It is not part of the file id, and a catalog remains valid if the scheme changes or a row has none.

| File size | Scheme name | Bytes hashed |
| --- | --- | --- |
| ≤ 64 MiB | `xxh3-128-full` | The entire file |
| > 64 MiB | `xxh3-128-sample-v1` | A length-prefixed sample: the file size as an 8-byte little-endian integer, then the first 1 MiB, 1 MiB centered on the midpoint, and the last 1 MiB. Short reads at the ends use the bytes that exist. |

XXH3-128 is the first hash, not a permanent choice. The scheme name is stored on the row that carries that fingerprint. Two fingerprints are compared only when those strings are equal. A different scheme, or a null fingerprint, is not evidence that the bytes differ. Confirmation is a digest comparison. Fingerprint equality never merges file ids.

The 64 MiB cutoff and the 1 MiB windows belong to `xxh3-128-sample-v1`. Tests pin that name to that layout. A different cutoff or window is a new scheme name. Rows already stored keep the name they were written with. No migration rewrites them, and opening the catalog does not recompute them.

### Full digest

The digest BPM computes by reading bytes is BLAKE3, algorithm `blake3`, wire form `blake3:<hex>`. Digests are stored as lowercase hex. Comparison is case-insensitive on input and lowercase on output.

On a posix location, `bpm scan` streams each selected location once through BLAKE3. Ingest reads a whole file only to test a duplicate: same size, same fingerprint scheme, and same fingerprint as a file already in the catalog. It hashes the new path and, when the existing file has no BLAKE3, that file's current bytes too. Matching BLAKE3 values add a location.

`bpm scan --md5` is a second pass that stores algorithm `md5`. Without the flag, scan does not compute MD5. Ingest does not compute it.

### Backend checksums

Some backends charge for a body download and also publish a checksum. S3 is the case this rule is written for. It stores one of ten checksums with the object. The ten include SHA-256 and XXH3. Ingest and scan trust the checksums the backend returns and do not download the body.

Each returned checksum is a `file_digests` row with `source = backend` and the provider's algorithm name. Size comes from the listing or from HEAD. A matching size and a matching current digest of the same algorithm add a location, with no download. If the backend returns no checksum, the object becomes a new file id. The same size as an existing file, with no comparable digest, is a possible duplicate and is not merged.

Downloading the body is an explicit flag on `bpm ingest` and on `bpm scan`. The spelling is TBD. With the flag, BPM streams the body, stores BLAKE3 with `source = computed`, and on ingest of a new object also stores a fingerprint. A posix location has no provider checksum, so those commands read bytes without that flag.

Drift against a trusted checksum compares what the backend returns now with the stored digest of that algorithm. A locally computed BLAKE3 is compared on its own row. `digest_mismatch` is set when any algorithm compared on that scan disagrees. `ok` requires every algorithm compared on that scan to match, or records the digest the first time one is stored.

### Path filters

The blacklist and the whitelist apply to the ingest walk and to which existing locations a scan checks. They do not delete a catalog row that a later filter would have skipped.

The built-in blacklist is the file names `.DS_Store` and `Thumbs.db`. It always applies, unless this run passes `--no-default-blacklist`.

Further patterns are globs. A pattern with no `/` matches the final path component, so `*.txt` skips that name at any depth. A pattern containing `/` is matched against the path relative to the walk root, or against the scan path as the whitelist is, and uses the same `*` and `**` rules.

`~/.bpm/config.toml` may set a global list:

```toml
blacklist = ["*.txt", "scratch/**"]
```

`--blacklist '*.txt,*.bak'` replaces that global list for one run. The patterns are comma-separated. A pattern that itself contains a comma is written in the toml, as one string in the array. Passing `--blacklist` does not drop the built-in names. `--no-default-blacklist` does. There is no per-catalog blacklist and no blacklist file discovered by walking up from the data.

The whitelist is zero or more glob patterns, passed as `--whitelist '*.fq.gz,*.bam'`. Both flags may be repeated, and their comma-separated lists are joined. `--blacklist ''` empties the global list for one run. With none set, every path that survives the blacklist is eligible. With one or more set, a path must match at least one pattern and must not be denied. `*` does not cross `/`. `**` does. On ingest the pattern is matched against the path relative to the walk root. On scan it is matched against the location path relative to the scan root, or against the full URI when the scan covers a whole backend.

### Ingest walk

1. Resolve the pathspec to a canonical directory. Refuse to start if it is not a directory.
2. Walk depth-first. Skip directory symlinks whose canonical target is outside the root, and record them as ingest errors of class `outside_root` (reported, not fatal). Skip symlink cycles.
3. Follow a symlink to a regular file. The walked path and the canonical path are two location candidates when they differ. A directory, whether reached directly or through a link, is walked by its canonical path, so every file under it is recorded under the path later commands resolve to.
4. Apply the blacklist and the whitelist. Each candidate is matched by its own path: relative to the root, or by its final name when a link target is outside the root. A link is skipped when its target is denied, even if the link's own name is allowed.
5. Skip any path whose `(backend, uri)` is already a location. Do not stat it for drift and do not hash it.
6. For each new regular file on a posix backend, record size, mtime, and fingerprint. If size, scheme, and fingerprint match an existing file, run the duplicate consultation above. On a checksum-bearing object backend, record size and the backend checksums, and skip the body unless the download flag is set.
7. A path that cannot be read, when a read was required, is an error and receives no fingerprint. It still gets a file row and a location, with the size and mtime stat returned, so a later scan can read it once it is readable. A path that cannot be stat'ed gets no row.
8. Write the ingest row as `complete`, or `incomplete` if any batch failed.

Ingest does not mark absent paths `missing`. That is scan.

Directory walk parallelism is an implementation choice. Batches of 1,000 files stay the commit boundary. The first implementation is single-threaded and walks each directory in name order.

Within one batch, two new paths with the same size and fingerprint are compared by BLAKE3 the same way, so copies ingested together become one file id. A new file whose BLAKE3 was computed is checked again under the write lock. If another ingest committed those bytes while this batch was hashed, the path becomes a location of that file. If the read that would confirm a fingerprint match fails, ingest reports a possible duplicate and stores the path with no fingerprint, as for any path it could not read. An existing file with no BLAKE3 is hashed only from a present location whose size and mtime still match what the catalog recorded. Bytes that changed since then are not the file the catalog describes, so such a candidate counts as one that cannot be hashed and is reported as a possible duplicate.

### Scan

Scan does not walk for new files. It selects location rows, applies the blacklist and the whitelist, and stats what remains. A posix location is streamed through BLAKE3. A checksum-bearing object location is checked from the checksums the backend returns, and the body is downloaded only when the flag is set.

A scan argument that is a backend name (`posix`, `s3`) selects that backend. Anything else is a path on the local filesystem, so a directory named `posix` is written `./posix`. A path that no longer exists still selects the locations recorded under it. That is how the old side of a move is scanned. Locations are read a page of 1,000 at a time, in `(backend, uri)` order, and each page is one committed batch.

| Invocation | Locations selected |
| --- | --- |
| `bpm scan` | Every location in the catalog, after the path filters |
| `bpm scan s3` | Locations whose backend is `s3`, after the path filters. A file that also has a `posix` location is not checked there |
| `bpm scan /data/run42` | Local-backend locations under that path, after the path filters |

The `s3` row is the selection rule for the later object-store milestone. The first milestone records only `posix` locations, so that invocation selects nothing until the backend exists. A path with no backend is the local filesystem in every milestone.

A missing URI sets `presence` to `missing`. A size or mtime change sets `stat_state` to `changed`, and `last_size` and `last_mtime` record the new observation. `changed` is sticky: a later scan that sees the same new stat does not clear it. Only acknowledge returns the location to `unchanged`, so the evidence of a change outlives the scan that found it. When the stat changed, or the file has no fingerprint yet, the fingerprint is recomputed from the same read. It is written to `files`, with the new size and mtime, only when those bytes hold the current BLAKE3. The size, mtime, and fingerprint on `files` describe the bytes of the current digest, and ingest matches duplicates on them. A copy that drifted must not move them away from its unchanged siblings. Acknowledge is what moves drifted bytes onto `files`. The digest comparison is independent, per algorithm, as in the backend-checksum rule above. Operator filters read these columns, so one location can be both `stat_changed` and `digest_mismatch`.

The first scan that reads a file records its BLAKE3, even when the stat changed since ingest. The location then shows both `stat_changed` and the recorded digest, and any later change to the bytes is `digest_mismatch`. Within one batch, locations whose stat did not change are applied first, so when a file has an unchanged copy, that copy supplies the first digest. An MD5 from `--md5` is recorded only beside a BLAKE3 that matched or was just recorded, in the same generation. A present location whose stat is unchanged but whose bytes have not been hashed is none of the four operator states. Query output shows it as `unverified`, and `--drift unverified` selects it.

| Operator state | Columns |
| --- | --- |
| `missing` | `presence = missing` |
| `stat_changed` | present, and `stat_state = changed` |
| `digest_mismatch` | `digest_state = mismatch` |
| `ok` | present, `stat_state = unchanged`, and `digest_state = match` |

Scan does not merge file ids. Merging a duplicate path onto an existing file id happens during ingest.

### Acknowledge

`bpm acknowledge FILE` names exactly one file, by file id or by the path of one of its locations. A directory is refused, and the command takes no second file, no backend, no glob, and no `--all`. Accepting drift is a decision about one file, so it is one command per file.

1. Read every present location of the file in full, with the catalog lock released: BLAKE3, the fingerprint, and MD5 when the file already has a current MD5 or `--md5` is passed. A location that is gone is recorded `missing`. A location that cannot be read fails the command.
2. If the present locations do not all have the same BLAKE3, refuse and list each location with its digest. Nothing is written. The operator restores or removes the wrong copy and runs acknowledge again.
3. If the BLAKE3 matches the current one and every location is already `ok`, report that there is nothing to acknowledge and write nothing.
4. Otherwise, in one write transaction: when the bytes are new, clear `current` on every digest of the file and insert the observed digests as generation `max(generation) + 1`. When they match the current BLAKE3, keep the generation and accept only the stat. Write the size, mtime, and fingerprint to `files`, and mark each present location `present`, `unchanged`, `match` with its new stat. A location found missing keeps `missing`, and after a new generation its `digest_state` returns to `unverified`, because those bytes were never compared with the new digest.

### Object storage later

Remote object stores, including S3, are a later Core milestone (PRD §3). They use the same file id and the same drift states as a filesystem location. The first milestone writes `posix` only. No acceptance test in that milestone covers S3. The backend half of PRD §4.15 scenario 15 runs when a second backend exists.

A new backend implements "list objects under a prefix, including size and any checksums" and, when a body read is required, "open a stream". It emits the same observation records. `file_locations.backend` and `uri` already distinguish a posix path from, for example, `s3://bucket/key`. The default for a backend that publishes checksums is to trust them. The download flag is what requests a local BLAKE3.

## 7. Query (UNSTABLE)

**Unstable (PRD §4.8).** `bpm query` is marked `*` in PRD §4.13. The methods and flag spelling below are a working sketch. `bpm sql`, in the next subsection, is a Core command and is outside this mark.

Semantic query methods compile to SQLite SQL inside the SQLite catalog implementation. They use explicit column lists and ordinary joins. SQLite-only syntax stays inside this module so a Postgres implementation can replace the function bodies without changing callers.

Selective queries are index lookups and are fast at the scale in PRD §2. `summary`, the unlinked-file anti-join, and possible-duplicate detection read whole tables. SQLite answers them in roughly half a second to a second on a catalog with 100 million metadata pairs (ADR 0001). That meets the PRD target.

The sketch uses `entities_under`, `files_under`, and `summary`. `impact` and `lineage` wait for PRD §4.7 and are not in this module yet.

Entity `--under` joins down the parent foreign keys from the addressed node and includes that node. File `--under` joins that set to `file_links` on `(node_type, node_id)`. A Program or Project is addressed by path. A Case, Sample, Raw Data, or Analysis is addressed by UUID, which the `entities` view resolves, or by a metadata path that matches exactly one node.

Metadata selectors are the three forms in PRD §4.4: `key:value`, `key:` (that key is present), and `:value` (that value is present on any key). A pair or a key-present selector is an index lookup on `entity_metadata (key, value)` or `file_metadata (key, value)`. The entity lookup returns `(node_type, node_id)` and then opens that table. A value-only selector that does not also fix the type reads every branch of the `entities` view. Values compare as strings. Repeated selectors are a conjunction. Core does not parse `embargo_until` as a date. A Govern reading of that key is PRD §5, which is unstable.

The flag spelling of `bpm query` is the working sketch in PRD §4.8. The flags can change without a new schema.

`summary` is a handful of grouped aggregations. The benchmark (PRD §4.15 scenario 31) times `files --under` a Project-sized subset and `entities --where` a selective key.

### SQL

`bpm sql` is local only. The default connection is read-only, so a stray write cannot succeed. `--write` opens a writable transaction and runs the statement. It does not append a `command_history` row. That recording waits for PRD §4.7. A statement the engine rejects rolls back and leaves the catalog unchanged.

### Canonical manifest JSON

**Unstable (PRD §4.10).** The layout below is the sketch in that section. It is not a settled format. Derived-from pairs in a manifest also wait for PRD §4.7.

Content body, key order alphabetical, arrays ordered as specified:

- `catalog_id`
- `entities`: `node_type`, id, path when the node has one, metadata map. Tree order: Programs and Projects by path, then remaining nodes by parent id and UUID
- `files`: id, digests (algorithm-tagged), size, links `{node_type, node_id, role}`, `unhashed`, in file-id order
- `derivations`: `{output, input}` pairs among included files, in output id then input id order

Links carry `node_type` and `node_id` because that pair is how a link names a node, and because Case, Sample, Raw Data, and Analysis have no path. A fingerprint is not an input to `content_id`.

Snapshot body is the content body plus, on each file, `locations` (backend, uri, presence, stat_state, digest_state) and the file's current drift summary.

`content_id` is SHA-256 of the canonical content bytes. `snapshot_id` is SHA-256 of the canonical snapshot bytes. Both are lowercase hex.

## 8. Web UI

`bpm serve` binds `127.0.0.1:3000` unless overridden. Resolution order is the flag, then the environment variable, then the default.

| Setting | Flag | Environment variable | Default |
| --- | --- | --- | --- |
| Host | `--host` | `BPM_HOST` | `127.0.0.1` |
| Port | `--port` | `BPM_PORT` | `3000` |
| Token | `--token` | `BPM_TOKEN` | unset |

If `--host` or `BPM_HOST` is set and neither token source is set, the process exits before binding. When a token is set, a login page posts the operator's entry and compares it to the configured value. A match sets an `HttpOnly` session cookie. A mismatch renders the login page again. The token is not logged and not stored in the catalog. With no token configured, the catalog routes are open, which is allowed only on the default host.

Core v1 opens the catalog read-only. Handlers are GET routes and return HTML. HTMX requests return a template partial.

| Route | Page |
| --- | --- |
| `GET /` | Home summary |
| `GET /login` | Token form, only when a token is configured |
| `GET /tree`, `GET /entities/{id}` | Tree and entity, read-only |
| `GET /files/{id}` | File detail: id, fingerprint, digests, locations, drift, and links. Derived-from waits for PRD §4.7 |
| `GET /search` | Query form and results partial. Filter spelling is the PRD §4.8 sketch |

Core v2 is the read-write UI. Its routes and behavior are TBD. v1 does not register POST handlers that mutate the catalog, including metadata edits, acknowledge, ingest, scan, and manifest build. The login form may POST the token. That request does not write the catalog.

Assets are the files committed under `assets/vendor/`: the HTMX runtime, the compiled Tailwind and DaisyUI stylesheet, and any other JS or CSS the pages use. The binary embeds those copies with `rust-embed` or `include_bytes!`. A build does not download them. Regenerating the stylesheet is optional, and the new file is committed back. Templates are Askama and compiled into the binary.

A startup log line states the bind address. It does not state the token.

## 9. CLI

Clap 4 derive parser. The binary name is `bpm`. The subcommands Core implements are the rows in PRD §4.13 whose Unstable cell is empty. `--catalog` is a global argument. `--format` applies to commands that print rows. On `bpm query` the flag set is still the §4.8 sketch.

Output goes to stdout. Diagnostics go to stderr. Exit status is `0` on success and non-zero on a rejected command. `bpm ingest` and `bpm scan` also exit non-zero when any path could not be read. The paths they could read are committed first.

`bpm delete` takes an entity, `--location PATH`, or `--file FILE`. A file is named by its id or by one of its location paths. `--file` refuses a file with links unless `--cascade` removes them too. None of these touch bytes on disk. `bpm link FILE ENTITY --role ROLE` and `bpm unlink FILE ENTITY` name the file the same way.

`bpm login`, `bpm logout`, and `bpm use` are marked `*` in PRD §4.13. Core does not register them. The token typed into `bpm serve` is not `bpm login`. When a Govern client exists, those commands with no server configured fail with a short message, and they do not report that a login succeeded.

A partial materialize that exits non-zero, writes a report beside the staged files, and leaves the manifest content body unchanged is the §4.10 sketch. It is not settled CLI behavior.

## 10. Govern seams

**UNSTABLE / TBD (PRD §5).** Everything in this section is the sketch kept next to that part of the PRD. It is not a Core design. Core does not build these modules, this database, or this remote API. The HTTP routes below are one transport sketch. Section 2 also leaves gRPC, or something similar, open.

The sketch adds modules. It does not fork `model`.

| Module | Responsibility |
| --- | --- |
| `auth` | OIDC code flow, PKCE, client credentials, session cookie |
| `rbac` | Role bindings per catalog, union of permissions, checks in middleware |
| `audit` | Append-only events in the operations database |
| `policy` | Pure function from a catalog snapshot plus a release's allow-list and cutoff to included, excluded, and impacted sets |
| `release` | State machine `draft → in_review → approved → materialized → revoked` |
| `review` | `PolicyReviewer` trait. First implementation calls the xAI API from the server |

The policy function reads entities, the metadata keys the sketch interprets (`consent` and `embargo_until`; further keys may be added later), file links, and derivations. It does not stat files and it does not call the model provider. Given the same snapshot and the same allow-list, it returns the same sets. A file stays in the package when any link the release uses attaches it to an included entity. It is withheld when every such link hangs off an excluded entity. A document linked to both a Project and a Case remains with the Project when that Case is excluded. Files outside the selection that are forward-reachable through derived-from from an excluded file are impacted and listed apart from ordinary exclusions. Tests for the Govern acceptance scenarios would run against this function with an in-memory SQLite fixture, without standing up OIDC. Those scenarios are PRD §5.8 and are not Core tests.

The AI reviewer receives the decision report and the policy text. Its output is stored as an artifact on the release. The approve transition checks the caller's role. It does not check that a review exists, so an officer can approve a small release without the model. The UI can require the review as a form of local policy later; the first rule is the role check, because a model outage must not trap a release.

### Operations database

`bpmd` keeps a second SQLite file, the operations database, separate from every catalog:

| Table | Contents |
| --- | --- |
| users | OIDC subject, display name, email |
| service_accounts | OAuth client id |
| role_bindings | user or service account, catalog id, role |
| sessions | web sessions |
| audit_events | the PRD audit fields |
| catalogs | registered catalog id, filesystem path, label |
| releases | state, selector, allow-list, cutoff, decision report, manifest content id, reviewer artifact |
| release_recipients | release id, name, institution, contact, destination description, materialized_at |
| policy_documents | catalog or program scope, text body, content hash |

Catalog *contents* stay in the per-initiative catalog file. The operations database stores the catalog's path and id so the server can open it. Role checks happen before any catalog call.

This split is the planning decision: one database file per catalog, plus users and audit in a side database.

### Trust boundary

Authenticated actions go through `bpmd`, which writes audit events and is the only principal that should be able to open the files. A process that opens a catalog file directly skips auth, policy, and audit. The deployment consequence is filesystem permissions: the catalog directory and the operations database are owned by the `bpmd` account, mode `0700` / `0600`. The architecture does not claim the database engine enforces row-level security.

OIDC authenticates. BPM's `role_bindings` table authorizes. Mapping an IdP group claim into a role is not part of the first Govern cut. An Admin assigns roles after the first login creates the user row.

### HTTP shape for the remote trait

This is the sketch if the transport is HTTP. Section 2 does not choose that. gRPC or something similar remains open and might carry large binary data more efficiently. In this HTTP sketch the routes are resources, not an argv tunnel. The CLI builds the same structs it would pass to `LocalSqlite` and the remote client posts them.

| Method and path | Trait group |
| --- | --- |
| `GET /api/catalogs` | list catalogs the caller may see |
| `POST /api/entities` | create |
| `PATCH /api/entities/{id}` | rename, reparent, metadata |
| `POST /api/ingests` | apply an ingest batch of new locations |
| `POST /api/scans` | apply digest and drift observations for locations already in the catalog |
| `POST /api/files/{id}/digests` | store a digest |
| `POST /api/links` | link |
| `POST /api/derivations` | derive |
| `POST /api/query` | semantic query |
| `POST /api/manifests` | build a manifest from a selector |
| `POST /api/releases` | Govern only |

The localhost UI does not go through these routes. It calls the trait. Keeping one HTTP API for the remote CLI and for `bpmd`'s own later needs is enough. Adding a second internal HTTP hop in `bpm serve` would only obscure errors.

## 11. Migrations, tests, and the benchmark

Schema version lives in `catalog_meta.schema_version`. On open, a writer runs ordered SQL migrations embedded in the binary until the stored version matches the code. Migrations are forward-only. A catalog written by a newer binary is refused by an older binary with a message that names both versions. The procedure, the SQLite `ALTER TABLE` limits, and the SQLite file format are in [Catalog migrations](migrations.md).

Core has no operations database. When that database exists, under the unstable Govern sketch, it migrates the same way with its own version.

Tests:

| Kind | What it covers |
| --- | --- |
| Model unit tests | Program and Project names, parent foreign keys, path formatting, rejection of `:` and `/` in metadata keys and values, rejection of a link or metadata row whose `(node_type, node_id)` is missing |
| Catalog integration tests | The PRD §4.15 scenarios that are not marked deferred or unstable, each against a temporary SQLite file |
| Ingest and scan fixtures | Directories of small files, a symlink, an unreadable file, a simulated move, a second path with the same bytes |
| Later milestones | Canonical manifest JSON and `--refresh-snapshot` when PRD §4.10 is finalized. Cycle rejection on derived-from edges when PRD §4.7 is built |
| Policy unit tests | Govern acceptance items 4–7, on the pure function, when that module exists. PRD §5 is unstable, so these are not Core tests |
| Benchmark | Synthetic 1,000,000-file catalog, timed queries from PRD scenario 31. Not part of the default `cargo test` run. The engine comparison that chose SQLite is a separate harness in `benches/engine_comparison` |

The benchmark builds rows with the library's insert path or a bulk loader that writes the same schema, so it measures the real tables. It is a binary under `cargo bench` or an example, and it prints elapsed times. It does not fail the build on a threshold in the first cut, because developer laptops vary. The threshold becomes a gate when there is a reference machine.

## 12. Postgres, when it is actually needed

SQLite is the catalog store for Core. The Govern sketch (PRD §5, unstable) also starts on SQLite, with a side operations database. Postgres is the exit when any of these become operational requirements:

- more than one server process must write the same catalog at the same time
- point-in-time recovery has to be handled by the database rather than by file backups of the SQLite files
- row-level security has to be enforced inside the database, because filesystem permissions around the server process are no longer a sufficient trust boundary
- the operators who will run it already back up Postgres and will not operate a file-per-catalog layout

Until then, adding Postgres adds a service the single-binary story was meant to avoid, and it splits every development setup. Postgres is not a dependency of Core.

The insulation is the catalog trait plus the discipline that SQLite-only SQL stays inside the SQLite module. The logical schema in section 5 maps onto ordinary Postgres tables: UUID, text, timestamps, booleans, and foreign keys up the node chain. The polymorphic `(node_type, node_id)` columns are ordinary text and UUID there too. The SQLite physical types (UUIDs and timestamps as `TEXT`, booleans as `INTEGER`) convert on copy. `STRICT`, `WITHOUT ROWID`, and pragmas are storage details that do not carry over and do not change the logical schema. Tables marked deferred or unstable in section 5 are not part of that mapping until their milestone is finalized.

A migration to Postgres would be a second implementation of the trait and a one-time copy from each SQLite file into tables keyed by `catalog_id`. An operations database, if Govern is built, can move on a different day than the catalogs. A later remote client would not change, because it talks to the trait.

In the Govern sketch, audit can stay an append-only table in whichever engine holds the operations database. A hash chain over audit rows is an extra integrity measure, not a prerequisite for those scenarios. Whether to add one is open question 1. It is not a Core decision.

## 13. Decisions

| Topic | Decision |
| --- | --- |
| Binary | Cargo package remains `bpm3`. The binary target is `bpm`. The package is not renamed: the names `bpm` and `bpm_next` belong to deprecated packages. |
| Process | One library, called in-process by `bpm` and `bpm serve`. A second binary, `bpmd`, is PRD §5, which is unstable. |
| Store | Embedded SQLite in WAL mode, one file per catalog, on a local filesystem. A side operations database is the Govern sketch, not a Core dependency. See [ADR 0001](../adr/0001-sqlite-catalog-store.md). |
| Seam | A catalog trait of semantic operations. The remote transport is not chosen: HTTP, or gRPC or similar (§2). SQL is local-only, and `--write` does not record history until PRD §4.7. |
| Nodes | Six tables. Each parent column is a foreign key to the table one step up, `ON DELETE RESTRICT`. `name` is a column on `programs` and `projects` only. Descendant queries join those parent keys. No closure table. |
| Attachments | One `files` table, with no node column. `file_links` and `entity_metadata` use `(node_type, node_id)`. That pair is not a foreign key. The library checks it. Ingest does not write links. |
| Digests | Rows in `file_digests`, one current row per algorithm, generation column is the history. Acknowledge clears every current digest for the file together. |
| Fingerprint | First scheme is XXH3-128, named on the row. Nullable. A new scheme does not rewrite existing rows or change file ids. Compared only when the scheme strings match. |
| Full digest | BLAKE3 when BPM reads bytes. `bpm scan --md5` stores MD5 as a second pass. A backend that publishes checksums is trusted. The flag that forces a download is deferred until the object-store stage. |
| Ingest commit | Batches of 1,000 files. The catalog lock is held only while a batch is committed. Hashing happens with the lock released. A per-run flock records that a long run is still alive. |
| Manifest mutation | **Unstable (PRD §4.10).** Sketch: content body immutable. Snapshot may be refreshed in place. |
| UI | Server-rendered Askama. HTMX, compiled DaisyUI CSS, and any other JS or CSS are vendored under `assets/vendor/` and embedded. Default `127.0.0.1:3000`. v1 is read-only. A non-default host requires a token. |
| Authz source of truth | **Unstable (PRD §5).** Sketch: BPM role bindings. OIDC proves identity. No IdP group mapping in the first cut. |
| Policy | **Unstable (PRD §5).** Sketch: pure function, snapshotted onto the release. A file on an included entity stays included. Reviewer cannot approve. |
| Bytes | Never stored in the catalog. Never proxied through the web UI. |
| Postgres | Documented exit with concrete triggers. Not a dependency of Core. |

## 14. Open technical questions

1. **Hash-chained audit.** Append-only plus file permissions is the current bar. A hash chain is easy to add later and harder to retrofit into an export an auditor already has. Decide before the first production Govern deployment, not before Core.
2. **Scan from a laptop against a server that cannot see those paths.** The batch operation on the trait allows it. The first Govern deployment does not expose it. Confirm that the first server really shares a filesystem with the data.
3. **Background ingest and scan in `bpm serve`.** Not part of the read-only v1 UI. If Core v2 starts those operations from the browser, the scheduling design (thread in-process versus a subprocess) is still open.
4. **One node table.** Merging the six node tables into one `nodes` table, keyed by the UUID with a `node_type` column, would make `(node_type, node_id)` on links and metadata an ordinary foreign key to `nodes.id`. The parent-type rule would then be a library check, or a trigger, rather than a foreign key. ADR 0001 lists this as a follow-up. It is not needed for the move to SQLite.
5. **Catalog backup.** A copy of the catalog file taken while a writer is active may miss committed rows that are still in the `-wal` file. Copying while no `bpm` process has the catalog open is safe. A `bpm backup` command using SQLite's online backup API or `VACUUM INTO` would remove that caveat. It is not in Core yet.
6. **Object-store download flag.** Ingest and scan trust a published checksum and download the body only when asked. The spelling of that flag is deferred until the object-store stage starts.
