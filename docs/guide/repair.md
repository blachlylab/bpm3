# Repairing a catalog

**Audience:** operators
**Companions:** [Product requirements §4.1](../product/prd.md), [Architecture overview §5](../architecture/overview.md)

`bpm` keeps a catalog consistent on its own. Every entity's parent exists. Every metadata row, link, location, and digest names an entity or file that exists. A catalog breaks only when something other than `bpm` writes the file. This guide covers how to notice that, how to look around, and how to fix it without losing more than you have to.

## When you need this

Either of these messages means the catalog is inconsistent:

```
bpm: catalog is inconsistent: 1 entity has no parent; run `bpm repair` to list the problems
bpm: catalog is inconsistent: project 0190…  has no parent program; run `bpm repair` to list the problems
```

The first comes from the check `bpm` runs before any command that walks or extends the entity tree. Those commands are `create`, `rename`, `meta`, and `query`. They refuse to run until the tree is fixed.

Four commands skip that check so you can inspect and fix a broken tree: `sql`, `repair`, `reparent`, and `delete`.

Data on disk is never at risk. `bpm` never deletes file bytes, and `bpm repair` never deletes a file row.

## How a catalog breaks

- **A SQLite tool with foreign keys turned off.** This is the usual cause. The `sqlite3` shell, a Python script, or a GUI browser that edits or deletes rows directly does not check foreign keys unless told to.
- **`bpm sql --write`.** Foreign keys are enforced, so it cannot remove a parent that still has children, or point a row at a missing parent or file. It can still write a metadata row or link whose `(node_type, node_id)` names no entity. Those pairs are not foreign keys.
- **Restoring a partial copy.** For example, copying a catalog file while a writer was running, without its `-wal` file.

## Emergency checklist

1. **Stop writers.** Let any `bpm ingest` or `bpm scan` finish or stop it. Close any other tool that has the file open for writing. `bpm serve` can keep running, because it only reads.

2. **Take a backup.** Writing it does not need the write lock:

   ```
   bpm sql "VACUUM INTO '/safe/place/catalog-before-repair.db'"
   chmod 600 /safe/place/catalog-before-repair.db
   ```

   `VACUUM INTO` writes a complete, consistent copy, including anything still in the `-wal` file. It creates the copy with your default file mode, so run the `chmod`. The catalog holds participant identifiers. Another option is to copy the catalog file together with its `-wal` file while no `bpm` process has it open.

3. **List the problems.**

   ```
   bpm repair
   ```

   This reads only. It prints `catalog is consistent` and exits 0, or it lists every problem and exits non-zero.

4. **Choose a fix for each problem.** See the fixes below, ordered from least to most destructive. Prefer the gentler ones: `bpm repair --apply` removes everything it lists.

5. **Check again.** Run `bpm repair` until it prints `catalog is consistent`.

## Reading the report

```
projects: 1 row(s), parent entity is missing
  01a1081a-8876-759a-8114-5053883be2b4
entity_metadata: 1 row(s), entity is missing
  sample 00000000-0000-7000-8000-000000000000 k
bpm: 2 row(s) break the catalog's integrity; see the repair guide, or `bpm repair --apply` removes them
```

Each problem names a table, a count, and what is missing, then lists up to 20 rows by key:

| Table | Problem | Key printed |
| --- | --- | --- |
| `projects`, `cases`, `samples`, `raw_data`, `analyses` | parent entity is missing | the entity's UUID |
| `entity_metadata` | entity is missing | `node_type node_id key` |
| `file_links` | entity is missing, or file is missing | `file_id node_type node_id` |
| `file_metadata` | file is missing | `file_id key` |
| `file_digests` | file is missing | `file_id algorithm generation` |
| `file_locations` | file is missing | `backend:uri` |
| `ingest_errors`, `scan_errors` | run is missing | `run_id uri` |

The orphan entities in the first five tables are what stop other commands, so fix those first.

## Looking around a broken tree

`bpm query entities --under` refuses to run until the tree is fixed. Use `bpm sql` instead. It is read-only unless you pass `--write`.

```
# The orphan, and the parent id it points at
bpm sql "SELECT id, program_id, name FROM projects WHERE id = '<uuid>'"

# Its children (cases under a project; use the next table down for other types)
bpm sql "SELECT id FROM cases WHERE project_id = '<uuid>'"

# Its metadata and its file links
bpm sql "SELECT key, value FROM entity_metadata WHERE node_id = '<uuid>'"
bpm sql "SELECT file_id, role FROM file_links WHERE node_id = '<uuid>'"
```

The parent column is `program_id` for a Project, `project_id` for a Case, `case_id` for a Sample, `sample_id` for Raw Data, and `raw_data_id` for an Analysis.

## Fixing an entity whose parent is missing

### A. Put the parent back

Use this when the parent was deleted by mistake and should exist. The orphan's parent column still holds the parent's UUID, so recreate a row with that exact id. Everything underneath reattaches at once.

```
bpm sql --write "INSERT INTO programs (id, name, created_at, updated_at)
  VALUES ('<parent uuid>', 'CLL',
          strftime('%Y-%m-%dT%H:%M:%fZ','now'), strftime('%Y-%m-%dT%H:%M:%fZ','now'))"
```

- Only Programs and Projects have a `name`. A Case, Sample, Raw Data, or Analysis row takes its own parent column instead.
- The restored row's own parent must exist. Foreign keys are enforced, so the insert fails otherwise. Restore from the top of the missing chain down.
- The parent's metadata is not restored. If you know it, set it again with `bpm meta set`. If you have a backup, the metadata is in that backup's `entity_metadata` table.

### B. Move the orphan under another parent

Use this when the old parent is gone for good but the orphan and everything under it should stay.

```
bpm reparent <orphan uuid> /CLL                # a Project goes under a Program
bpm reparent <orphan uuid> /CLL/WES-relapse    # a Case goes under a Project
bpm reparent <orphan uuid> <sample uuid>       # Raw Data goes under a Sample
```

The new parent must be the type one level up, as for any reparent. The orphan keeps its UUID, its descendants, its metadata, and its file links.

### C. Delete just that subtree

Use this when the orphan and everything under it should go, but you want to choose each one yourself rather than apply every listed fix.

```
bpm delete --cascade <orphan uuid>
```

Without `--cascade`, `delete` refuses while the orphan has children or file links. Cascade removes the descendants, their metadata, and their file links. Linked files stay in the catalog as unlinked files.

### D. Restore from a backup

If you have a copy from before the damage, from step 2 of an earlier repair or your own backups, copying it back may be the simplest fix. Anything written after that copy is lost. Run `bpm repair` on the restored file before relying on it.

### E. Remove everything listed

```
bpm repair --apply
```

Use this when nothing in the report is worth keeping, or after you have handled the entities you care about with A–C.

In one transaction it removes:
- every orphan entity with all of its descendants, their metadata, and their file links;
- metadata and link rows that name a missing entity;
- metadata, digest, location, and link rows that name a missing file;
- error rows that name a missing run.

It commits only if the catalog checks clean afterwards. Otherwise nothing changes. It prints what it removed, including how many descendants went with the orphans.

## Rows that name a missing entity or file

These do not stop other commands, but `bpm repair` reports them.

- **Metadata or a link on a missing entity:** nothing can use these rows. If the entity was deleted by mistake, use fix A, with the UUID from the key. The rows reattach to it.
- **Metadata, digests, links, or locations on a missing file:** the file row is gone and `bpm` cannot recreate it with the same id. Remove these rows with `bpm repair --apply`. The bytes are still on disk. Ingesting their path again gives them a new file id.
- **Error rows on a missing run:** history only. Remove them.

## After a repair

- Files whose links were removed stay in the catalog, unlinked.
- Run `bpm repair` once more and keep the backup from step 2 until you have checked the catalog.
- If the damage came from a third-party tool, turn on foreign keys in that tool (`PRAGMA foreign_keys = ON;` at the start of each session) or use `bpm sql`.
