# Implementation plan

**Status:** Draft
**Date:** 2026-09-30
**Companions:** [Product requirements](product/prd.md), [Architecture overview](architecture/overview.md)

This is the build order for the `bpm` binary. Behavior is the PRD. Mechanism is the architecture overview. A stage does not add product rules, and it does not implement a section that is still marked unstable or deferred.

PRD §4.16 is what the Core release contains. The order below is the order to build it. That section lists import and manifests before the web UI. They wait here, because §4.9 and §4.10 are still unstable. The read-only UI does not need them.

One Cargo package. The modules are the ones in the architecture overview: `model`, `catalog`, `ingest`, `query`, `cli`, and `web`. The catalog trait is the seam. SQL stays inside the SQLite implementation.

## What "done" means

A stage is done when the §4.15 scenarios named for it pass against a temporary catalog. Each of those scenarios is an integration test. Scenarios marked deferred or unstable are not part of the check. The benchmark (scenario 31) is a gate before the web UI, not a gate on the first usable catalog.

## 1. Catalog notebook

PRD slice A. Scenarios 1–9, 25, 27, and 30.

- `bpm init`, catalog resolution, file modes, WAL mode and the `BEGIN IMMEDIATE` write lock, and the schema migrator in [Catalog migrations](architecture/migrations.md).
- The six node tables, metadata, rename, reparent, and delete. UUIDv7 comes from the library.
- `bpm sql`, read-only unless `--write`. No command history.
- Enough of `bpm query` to answer the selectors in §4.4 for the scenarios above. The flag spelling is the §4.8 sketch and may change. Impact and lineage are not in this stage.

This stage is a notebook for Programs, Projects, and the nodes under them. It does not ingest files.

## 2. File indexer

PRD slice B, local filesystem only. Scenarios 10–21, except the object-store half of scenario 15.

- Ingest, the first fingerprint scheme (the scheme name stored on the row), BLAKE3 on scan and on duplicate confirmation, locations, drift, acknowledge, link, and unlink.
- The catalog lock is held only around each committed batch. Hashing the next batch happens with the lock released. A per-run flock is how a crash is detected.
- The built-in denylist, a global list in `~/.bpm/config.toml`, `--denylist` for one run, and the glob whitelist.
- `bpm scan --md5` when an archive requires MD5. It is off unless the flag is passed.

After this stage the benchmark catalog can be loaded. Scenario 31 runs before stage 4.

## 3. Object stores

A later Core milestone (PRD §3), built on the same file id, drift states, and link rules as the local filesystem. The object-store half of scenario 15, plus ingest and scan of an S3 prefix.

- Trust a checksum the backend publishes. Do not download the body unless asked.
- The spelling of the download flag is deferred until this stage starts.

## 4. Read-only web

PRD slice D, Core v1. Scenario 29.

- `bpm serve` on `127.0.0.1:3000`, with the host and token rules in §4.11.
- Pages for the home view, the tree, one entity, one file, and search. The pages do not write. Derived-from is absent.
- HTMX, CSS, and any other front-end files are vendored under `assets/vendor/` and embedded.
- Search uses the same query sketch as the CLI.

Read-write pages are Core v2. They have no stage until that UI is specified.

## 5. Import, manifests, and materialize

Starts when PRD §4.9 and §4.10 are no longer marked unstable. Scenarios 23, 24, and 26.

The sketches in those sections are not a build spec. This stage is not scheduled before that review.

## 6. Provenance

Starts when the capture and storage in PRD §4.7 are specified. Scenarios 22 and 28.

Derived-from edges, impact, lineage, and command history. `bpm sql --write` records a history row in this stage, not before.

## 7. Govern

Not scheduled. PRD §5 is unstable. When that section is revised, `bpmd` is a second binary on the same library. The transport between `bpm` and `bpmd` is still an open choice: HTTP, or gRPC or something similar.

## Not in these stages

- A closure table, or a shared id registry for nodes.
- A second fingerprint scheme. The stored scheme name is what makes a later one possible.
- Postgres.
- Parsers for FASTQ, BAM, VCF, or SVS headers.
- A read-write web UI.
