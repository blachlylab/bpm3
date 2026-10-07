# ADR 0002: Link templates

**Status:** Accepted
**Date:** 2026-10-07
**Supersedes:** `bpm link FILE ENTITY --role ROLE`, one file per command
**Guide:** [Linking files](../guide/link.md)

## Decision

`bpm link` takes one target template and any number of files:

```
bpm link --to TEMPLATE [--match RE | --match-path RE] [--table FILE --join COL=TEMPLATE]
         --role ROLE [--new-role] [--set-role] [--relink] [--expect TYPE=N[..M]]...
         [--strict] [-n] [--detail] [-y] FILE|LOCATION...
```

A template is an address followed by typed steps, such as `case[subject_id:{1}]?`. Each step names the node type one level down, the pairs that select it, and a mode. No mode means the node must exist. `?` uses the one match, or creates it when there is none. `+` creates a new node, one per distinct parent and pairs within the run. A node a step creates is given exactly that step's pairs. Placeholders come from the pattern's groups, the mapping table's columns, and two built-ins.

`bpm unlink` takes the same selection and refuses `?` and `+`. `bpm undo RUN` reverses one link run.

## Why one template instead of `--upstream` and `--downstream`

The first sketch had two flags: an upstream path to search, and a downstream type to create, each with its own create switch. When a run touches one existing node and creates nothing, nothing says whether that node is upstream or downstream. The two flags also mixed two questions: where the search starts, and which levels may be created. Putting the mode on each step answers both in one place. The file attaches to the last node of the path, whatever the run creates on the way.

## Why every level is written

An earlier version filled in skipped levels: `case[…]/analysis+` implied `sample?/raw_data?`. That made the template hide part of the tree it builds. It meant a mistyped step type created extra structure instead of failing. And a recorded command needed the fill rule to explain what it had done. Writing `sample?/raw_data?` costs a few characters. The error for a skipped level prints the full template to paste.

## Why `?` and not "the first one"

`/…/case/0`, meaning the first Sample or a new one, was considered for two assays from one Sample linked in two runs. `sample?` covers that case: the second run finds the only Sample. When there are two, `?` fails instead of choosing, because choosing silently is the error the operator needs to see. Pairs in the brackets tell siblings apart.

## Why the pairs set the cardinality

Whether R1 and R2 get one Raw Data node each, or share one, is decided by whether the read capture appears in that step's pairs. No separate flag is needed, and the node that results is addressable by the same pairs later.

## Why plan, confirm, and apply in one command

A plan file applied by a second command, as Terraform does it, was considered and rejected as too heavy for a CLI that is also used for one file. Instead, each run plans against a read snapshot and prints the plan. A run that creates entities then asks on a terminal, or needs `--yes` without one. Finally the run plans again under the write lock and writes only if that plan is identical to the one shown. The run is one transaction.

## Why undo tags rows instead of logging them

Link runs write `run_id` on the link rows and entity rows they create, and record the old role of any link they change. Undo deletes by that tag. It refuses when later work, such as a child, a link, a metadata edit, or a later role change, depends on the run. That is exact for everything a run can write, and it costs one column per row rather than a log of each link. To keep it exact, a link run never changes an existing link silently: a different role needs `--set-role`, which is what records the old role.

## Consequences

- The single-file `bpm link FILE ENTITY --role ROLE` is gone. Its replacement is `bpm link --to ENTITY --role ROLE FILE`. `bpm unlink FILE ENTITY` becomes `bpm unlink --to ENTITY FILE`.
- Linking an existing pair with a new role no longer replaces the role. It fails unless `--set-role` is passed.
- Roles are spelled `[a-z0-9_-]+`. The catalog keeps a list of known roles. A new role needs `--new-role` once. Migration V003 seeds the list with `data`, `index`, `document`, `report`, `qc`, `checksum`, and every role already in use.
- `--match` sees the file name. `--match-path` sees the path relative to the location operand. A location operand that is not itself a location reaches every catalog location below it.
- Schema version 3 adds `run_id` to `file_links` and to the Case, Sample, Raw Data, and Analysis tables. It also adds the `link_runs`, `link_role_changes`, and `link_roles` tables.

## Deferred

- Reporting files on disk under a directory operand that are not in the catalog. Which blacklist and whitelist would apply is unsettled.
- A map from a captured value to a role (`--role-map bam=data,bai=index`).
