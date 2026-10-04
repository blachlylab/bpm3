# Scenario 31 benchmark

**Date:** 2026-10-04
**Harness:** [`benches/scenario31.rs`](../../benches/scenario31.rs)
**Raw data:** `scenario31.json`

PRD §4.15 scenario 31 is the gate before the read-only web UI (stage 4). A
synthetic catalog of 1,000,000 files and 5,000,000 metadata entries runs
`files --under` a Project and `entities --where` a metadata key. The target is
interactive response, a few seconds, on one workstation.

**Result: passes.** Through the CLI, `files --under` a Project of 100,000 files
takes 0.8 to 1.1 s and `entities --where` takes 0.06 s.

## Method

- Apple M1 Pro, 10 cores, 32 GB, macOS. Release build, bundled SQLite.
- `Catalog::init` creates the catalog, so it carries every real migration (V002).
  Rows are bulk-loaded into that schema, followed by `ANALYZE`. Building takes
  about 55 s. The file is 1.9 GB.
- Tree: 1 Program, 10 Projects, 10,000 Cases, 20,000 Samples, 40,000 Raw Data,
  29,989 Analyses. That is 100,000 entities.
- Metadata: 5,000,000 entity pairs, 50 per entity. Cases carry a unique
  `subject_id`, Samples `sample_kind`, Raw Data `assay`. The generic keys `k00`
  to `k49` have between 5 and 100,000 distinct values.
- Files: 1,000,000, each with one location, one current BLAKE3, and one `data`
  link to a Raw Data node. 25 files per Raw Data, 100,000 per Project.
- Each timed query does what `bpm query` does: open read-only, run the tree
  check, query, and render the table. "First" is the first of three runs and
  "best" the fastest. The page cache was warm; a cold first read is slower
  (the engine comparison saw about 2 s for a cold `files --under`).

```
cargo bench --bench scenario31 -- --rebuild --json docs/benchmarks/scenario31.json
```

## Results

| Query | Rows | Best |
| --- | ---: | ---: |
| `files --under` a Project | 100,000 | 821 ms |
| `entities --where subject_id:<one Case>` | 1 | 57 ms |
| `files --under` a Case | 100 | 59 ms |
| `entities --where k03:v42` (1,000 values) | 100 | 62 ms |
| `entities --under` a Project `--type case` | 1,000 | 79 ms |
| `files --digest blake3:<one file>` | 1 | 24 ms |
| `files --unlinked` | 0 | 360 ms |
| `files --drift missing` | 0 | 824 ms |

End to end through `target/release/bpm` (process start, query, output to
`/dev/null`): `files --under /BENCH/P03` 0.83 to 1.05 s (1.32 s with
`--format json`), `entities --where subject_id:CLL-004243` 0.06 s.

## What the first run found

The first run failed the target. Every command that resolved an entity took
about 4 s, and `files --under` a Project took 7 to 9 s. Two causes, both in the
library, not in SQLite:

1. **The in-memory entity index loaded every metadata pair.** Stage 1's
   `load_index` read all 100,000 entities and all 5,000,000 metadata rows to
   resolve one address. It now loads the tree only. A metadata selector is a
   lookup on the `(key, value)` index, and metadata is read only for the rows a
   command returns. That alone brought entity queries from 4 s to 60 ms.
2. **File details were read with four statements per file.** `query files` now
   puts the result ids in a temporary table and reads each detail table in one
   join. The temporary table has no statistics, so the joins use `CROSS JOIN`
   to keep it as the outer loop. Without that, the planner scanned the
   million-row tables and a one-file query took a second.

The tree itself is not the slow step. Loading 100,000 entities and checking
their parents takes about 50 ms, so the overview's condition for adding a
closure table is not met.

## Still slow, and why it is acceptable

`files --unlinked` and `files --drift STATE` read a whole table: an anti-join
over 1,000,000 links, and a scan of 1,000,000 locations. They take 0.4 and
0.8 s. Both are interactive. A partial index on the drift states would make
`--drift` an index lookup if it matters for the UI.
