# Linking files

**Status:** Draft
**Date:** 2026-10-07
**Companions:** [Product requirements](../product/prd.md) §4.5 and §4.13, [ADR 0002](../adr/0002-link-templates.md)

`bpm link` attaches files that are already in the catalog to entities. It links one file to one entity, or a thousand files to a thousand entities it creates on the way, in one command and one transaction. `bpm unlink` takes the same arguments and removes links. `bpm undo` removes everything one link run wrote.

Ingest first. `bpm link` reads the catalog, not the filesystem, so a file that has not been ingested is not there to link.

```
bpm ingest /data/run42
bpm link --to '/CLL/WES/case[subject_id:{1}]?/sample?/raw_data[read:{2}]+' \
  --match '([A-Za-z0-9]+)_(R[12])\.fq\.gz' --role data /data/run42
```

Quote the template and the pattern. Both contain characters the shell would read.

## One file

```
bpm link --to /CLL/WES --role document /data/irb/approval.pdf
bpm link --to /CLL/WES/subject_id:CLL-001 --role document consent.pdf
bpm link --to 01a116f9-9f5e-70ec-a2c9-b393d8bb4c4e --role data S1_R1.fq.gz
bpm unlink --to /CLL/WES /data/irb/approval.pdf
```

`--to` is any address `bpm` already accepts: a Program or Project path, an entity UUID, or a metadata path that matches exactly one entity. The file is a path to one of its locations, or its file id.

## The template

`--to` is an address followed by typed steps. Each typed step is one level down the tree.

```
/CLL/WES/case[subject_id:{1}]?/sample?/raw_data[read:{2}]+
└─ anchor ┘└──── step ─────────┘└ step ┘└────── step ──────┘
```

The anchor is resolved the way any address is, and must match exactly one entity. A step is a node type, optional `[key:value, …]` pairs, and a mode:

| Step | Meaning |
| --- | --- |
| `case[subject_id:S1]` | The one Case under the parent with that pair. None, or more than one, fails the file. |
| `case[subject_id:S1]?` | That Case if there is one. If there is none, create it with `subject_id=S1`. More than one fails. |
| `raw_data[read:R1]+` | A new Raw Data node with `read=R1`. Files in this run with the same parent and pairs share it. |
| `sample?` | The only Sample under the parent, or a new one if there is none. |

The rules:

- **The pairs are both the match and the new node's metadata.** A node a step creates gets exactly the pairs written in its brackets. A step that may create (`?` or `+`) needs full `key:value` pairs. A step that must exist may also use `key:` (key present) and `:value` (value on any key).
- **The pairs decide how many nodes a step makes.** `raw_data[read:{2}]+` makes one node per sample and read, so R1 and R2 get one each. `raw_data+` makes one node per sample, and both reads share it.
- **Every level is written.** A step must be a child of the step before it. `case[…]/analysis+` fails, and the message spells out the full template: `case[…]/sample?/raw_data?/analysis+`. Nothing is filled in for you.
- **Programs and Projects are named in the path,** never created by a link.
- **Once a typed step appears, only typed steps follow.**
- **Nothing exists under a node that is new in this run,** so a must-exist step after a `+` step is refused.
- **Separators:** pairs are separated by commas, and spaces around them are ignored. A literal comma cannot appear inside a pair, but a placeholder value may contain one.
- **The word after a Program is a Project name,** so `/CLL/case` is a Project called `case`. A typed step goes after the Project: `/CLL/WES/case`.

`?` does not mean "first." When a Case has two Samples, `sample?` stops with an ambiguous-match error, because picking one silently is the mistake you would want to hear about. Tell them apart with pairs: `sample[tissue:tumor]?`.

## What to link: operands

Operands are file ids or catalog locations:

- **A file id** reaches every location of that file.
- **A path that is a location** reaches that location.
- **A path that is not a location** is treated as a directory. It reaches every location in the catalog below it, at any depth. Prefixes stop at a path component, so `/data/run1` does not reach `/data/run10`.
- **An operand that reaches nothing** fails the command, and the message says to run `bpm ingest`.
- **Object-store URIs** such as `s3://bucket/prefix` are refused until the object-store milestone.

A file reached through two locations or two operands is linked once. If the two locations would give it different targets or roles, the run fails and names both.

## Placeholders

`{…}` in the template and in `--role` is filled per file.

| Placeholder | Value |
| --- | --- |
| `{1}`, `{2}`, … | Numbered groups of the `--match` or `--match-path` pattern. `{0}` is the whole match. |
| `{name}` | A named group, `(?<name>…)`, or a column of the `--table` |
| `{basename}` | The file's name |
| `{relpath}` | The path relative to the operand |

Rules:

- **Literal braces:** write `{{` and `}}`.
- **Undefined names fail before anything is read.** A name defined twice also fails, for example a capture and a table column that are both `subject`.
- **Values cannot contain `/` or `:`.** These would change the shape of a path or a pair, so a file whose value contains either fails.
- **A group that did not take part in the match has no value.** A file that uses it fails.

## Patterns

- **`--match REGEX`** must match the file's whole name, as if it were written `^(?:REGEX)$`.
- **`--match-path REGEX`** must match the whole path relative to the location operand, so it can capture directory names:

```
bpm link --to '/CLL/WES/case[subject_id:{subj}]?/sample?/raw_data?/analysis[pipeline:bwa]?' \
  --match-path '(?<subj>[^/]+)/aln/[^/]+\.bam' --role data /data/aligned
```

A file the pattern does not match is listed in the plan and left alone. `--strict` makes it an error.

## Mapping tables

When the file name does not carry the id you want to file under, a table supplies it.

```
barcode   subject  tissue
BC01      S1       tumor
BC02      S1       normal
```

```
bpm link --to '/CLL/WES/case[subject_id:{subject}]?/sample[tissue:{tissue}]?/raw_data+' \
  --match '(BC[0-9]+)\.fq\.gz' --table samples.tsv --join 'barcode={1}' --role data /data/run42
```

- **File format.** A `.csv` file is comma-separated. Anything else is tab-separated with no quoting. The first row names the columns.
- **Columns become placeholders.** Every column whose name is letters, digits, and `_` becomes a placeholder.
- **`--join COLUMN=TEMPLATE` picks the row.** Each file uses the one row whose COLUMN equals TEMPLATE filled in. TEMPLATE may use the pattern's groups and the built-ins, but not other columns. `--join 'barcode={basename}'` needs no pattern at all.
- **Join keys are unique.** A key that appears on two rows fails before anything is linked. A file whose key has no row fails the run.
- **Unused rows are reported.** Rows no file reached are listed, and `--strict` makes them an error.
- **The table only places files.** Columns are not written as metadata. Use `bpm meta set` or, later, `bpm import` for that.
- **`--role` still comes from the command line.** It can be a constant, or a column such as `--role '{role}'` when the table has one.

## Roles

A role says why the file is on the entity: `data`, `index`, `document`, `report`, `qc`, `checksum`. The vocabulary is open, with guardrails so it stays one vocabulary:

- **Spelling:** a role is lowercase letters, digits, `_`, and `-`.
- **The catalog keeps the roles it has seen.** A new catalog starts with the six above. A catalog that already had links knows their roles as well.
- **`--new-role` adds a role.** A role not on the list fails unless the run passes `--new-role`, which then adds it. A typo does not quietly start a second spelling.
- **Changing a role needs `--set-role`.** Linking the same file to the same entity with another role fails without it.
- **`bpm query summary` counts links by role,** so a stray spelling is easy to spot.

## The plan, and applying it

Every run prints a plan first:

```
7 files reached, 6 matched, 1 not matched
1 not matched by the pattern:
  /data/run42/notes.txt
link 6, already linked 0, role changes 0
create 3 case, 3 sample, 3 raw_data, 3 analysis
roles: data 6
```

- **`-n` / `--dry-run`** prints the plan and stops.
- **`--detail`** adds one line per file: action (`link`, `already`, `set-role`, `skip`, `unlink`), location, target, and role.
- **Confirmation.** When the plan writes anything (links, role changes, or entities), `bpm` asks `Apply? [y/N]` on a terminal. With no terminal it refuses, unless `--yes` is passed. A run with nothing to write does not ask.
- **One transaction.** The run either writes everything or nothing. Between the plan and the write, `bpm` plans again under the write lock. If the catalog changed in between, it writes nothing and says so.
- **The result line.** A run that writes prints `link run <id>: …`. That id is what `bpm undo` takes.

## Checks

A run fails, and writes nothing, when:

- an operand reaches no catalog location;
- a placeholder is undefined, or a value is empty or contains `/` or `:`;
- an anchor or a must-exist step matches nothing, or more than one entity;
- a `?` step matches more than one entity;
- a step's type is not a child of the entity above it;
- a role is misspelled, or new without `--new-role`;
- a link exists with another role and `--set-role` was not passed;
- a file's locations disagree about where it goes;
- an `--expect` count is not met;
- with `--strict`, a file does not match the pattern or a table row matches no file.

Every problem is listed, up to the first twenty, so one run shows them together.

The plan also notes existing entities a must-exist step could have reached that got no file: `note: 88 existing case under the same parents got no files`. A note does not stop the run.

### `--expect TYPE=N`

`--expect` checks the shape of what you link. Write `TYPE=N`, or `TYPE=N..M` for a range. Every TYPE node the run touches must then get that many files beneath it. TYPE must be a step in the template. The count includes files that are already linked, so a rerun gives the same answer. Repeat the flag to check several levels.

- **Missing mate:** `--expect sample=2`. With one FASTQ per Raw Data node, a lost R2 does not show at the Raw Data level. It shows as a Sample with one file instead of two.
- **Collapsing lanes:** `--expect raw_data=1`. With `S1_L001_R1` and `S1_L002_R1`, a pattern that does not capture the lane puts both files on `raw_data[read:R1]`. The check catches it, and the fix is `raw_data[read:{read}, lane:{lane}]+`.

### Reruns

Running the same command again is safe:

- **Links that exist are counted** as `already linked`, and nothing is written for them.
- **Files already placed are skipped.** A file is skipped when it is already linked to a node of the target type under the anchor and the run would create a node for it. Without this rule, a rerun with a `+` step would build a second tree. `--relink` turns the rule off. That is for a file that really belongs on two such nodes, such as one control shared by several analyses.

## Recipes

**About 1,000 BAMs, no Cases yet.** The BAM and its index share an Analysis node:

```
bpm link --to '/P/J/case[subject_id:{1}]?/sample?/raw_data?/analysis[pipeline:bwa]?' \
  --match '([A-Za-z0-9]+)_xxxx\.ba[mi]' --role data /data/bams
```

To give the index the role `index`, run it as two passes. The second pass finds the nodes the first one made, because every step there is `?`:

```
bpm link --to '…/analysis[pipeline:bwa]?' --match '([A-Za-z0-9]+)_xxxx\.bam' --role data /data/bams
bpm link --to '…/analysis[pipeline:bwa]?' --match '([A-Za-z0-9]+)_xxxx\.bai' --role index /data/bams
```

**The same, with the Cases already there.** Drop the `?` from the case step: `case[subject_id:{1}]`. A subject with no Case fails the run and is named.

**About 2,000 FASTQs, one Raw Data node per file.** This is the recommended practice, and it matches the GDC model:

```
bpm link --to '/P/J/case[subject_id:{1}]?/sample?/raw_data[read:{2}]+' \
  --match '([A-Za-z0-9]+)_xxxx_(R[12])\.fq\.gz' --role data --expect sample=2 /data/fastq
```

A pair on one Raw Data node is still allowed: `raw_data[assay:WES]+` with `--expect raw_data=2`.

**One file onto an existing Case:**

```
bpm link --to '/P/J/case[subject_id:CLL-001]' --role document /path/to/file.xlsx
```

**WES and WGS from the same Sample, in two runs.** The first run creates the Sample. The second run finds that it is the only one and reuses it:

```
bpm link --to '/P/J/case[subject_id:{1}]?/sample?/raw_data[assay:WES]+' --match '…' --role data /data/wes
bpm link --to '/P/J/case[subject_id:{1}]?/sample?/raw_data[assay:WGS]+' --match '…' --role data /data/wgs
```

**Tumor and normal:** `case[subject_id:{1}]?/sample[tissue:{2}]?/raw_data[read:{3}]+`.

**Several projects from one directory:** `/P/{proj}/case[subject_id:{subj}]?/…` with named groups. The Projects must exist.

**Project paperwork:** `bpm link --to /P/J --role document /data/irb`.

## Unlink

`bpm unlink` takes `--to`, `--match`, `--match-path`, `--table`, `--join`, `-n`, and `--detail`. Every step must exist, so `?` and `+` are refused. A file that is not linked to its target fails the run. Unlink removes links only. Entities stay, and so do file rows.

## Undo

```
bpm undo --list
bpm undo 01a116f9-9f5e-70ec-a2c9-b393d8bb4c4e -n
bpm undo 01a116f9-9f5e-70ec-a2c9-b393d8bb4c4e
```

`bpm undo RUN` reverses one link run:

- it removes the links the run added;
- it removes the entities the run created, with the metadata the run wrote on them;
- it puts back the roles the run changed.

File rows and bytes are never touched. Undo asks before it deletes entities, and `--yes` skips the question.

Undo refuses, and lists why, when later work depends on the run:

- an entity the run created has a child, a link, or a metadata value that came later. If that later work came from another link run, the message names it: undo that run first.
- a role the run changed has been changed again since.

A run is undone once. Undoing an undo is not supported. Re-run the link instead.

Undo needs only what each row records: link runs tag the links and the entities they write with the run id, and keep the old role of each link they change. Links and entities written by `bpm create`, by the single-file form before this release, or by `bpm sql` carry no run id and are never touched by undo.

## Not yet

These were discussed and set aside for a later decision:

- **A not-ingested check.** Counting files on disk under a directory operand that are missing from the catalog. The open question is which blacklist and whitelist to apply, since link takes neither.
- **Mapping a capture to a role.** For example `--role '{ext}' --role-map bam=data,bai=index`, which would replace the two-pass recipe above.
