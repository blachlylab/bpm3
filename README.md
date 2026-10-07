# bpm

`bpm` is the local binary for Biodata Project Manager. It catalogs biomedical files and the program, project, case, sample, assay, and analysis they belong to. The bytes stay in the storage that already holds them.

The product contract is [docs/product/prd.md](docs/product/prd.md). How the catalog is built is [docs/architecture/overview.md](docs/architecture/overview.md). This page is the short operator guide, starting with the settings file those two documents specify and `bpm --help` only names.

```
cargo build --release
# binary: target/release/bpm
bpm init
```

`bpm init` creates `~/.bpm/default.db`. Another catalog is `--catalog PATH`, or the `BPM_CATALOG` environment variable.

## Path filters

`bpm ingest` and `bpm scan` share two filters:

- A **blacklist** of globs to skip.
- A **whitelist** of globs. With none set, every path that survives the blacklist is eligible. With one or more set, a path must match at least one of them.

The blacklist is the built-in names plus an extra list. The whitelist is its own list. Each list lives in `~/.bpm/config.toml`, or, for one run, in the matching flag.

Filters choose which paths that run considers. A path already in the catalog stays there when a later run would have skipped it.

### `~/.bpm/config.toml`

The file is optional. When it is absent, both lists are empty and the built-in names still apply. `bpm ingest` and `bpm scan` read the file when at least one of `--blacklist` and `--whitelist` is absent. When both flags are set, the file is not read.

```toml
# ~/.bpm/config.toml
blacklist = [
  "*.txt",
  "*.bak",
  "scratch/**",
]
whitelist = [
  "*.fq.gz",
  "*.bam",
]
```

`blacklist` and `whitelist` are arrays of strings. Either key may be omitted. These two files make the command fail, and the error names the config path and the key:

```toml
# one string, where an array is required
whitelist = "*.fq.gz"

# an array, but not of strings
blacklist = [1, "*.txt"]
```

A file that is not valid TOML fails the same way. Any other key is left unread. Catalog selection stays on `--catalog`, `BPM_CATALOG`, and `~/.bpm/default.db`. Named catalogs, when recorded, go in `~/.bpm/context.toml`.

A pattern that itself contains a comma is one string in the array. The command-line form splits on commas, so that pattern stays in the file:

```toml
blacklist = ["report,final.txt", "scratch/**"]
whitelist = ["*.fq.gz"]
```

### Globs

A pattern with no `/` matches the file name at any depth, so `*.txt` skips `notes.txt` and `lane1/notes.txt`.

A pattern with `/` matches the path relative to the directory named on the command. For `bpm ingest /data/run42`, the path `scratch/x/y.fq` is tested as `scratch/x/y.fq`.

`*` does not cross `/`. `**` does.

| Pattern | `notes.txt` | `lane1/notes.txt` | `scratch/x/y.fq` | `keep/scratch/y.fq` | `bams/S1.bam` | `bams/sub/S1.bam` |
| --- | --- | --- | --- | --- | --- | --- |
| `*.txt` | skip | skip | | | | |
| `scratch/**` | | | skip | keep | | |
| `bams/*.bam` | | | | | match | |
| `bams/**` | | | | | match | match |

An empty cell means that pattern neither skips nor, on its own, selects the path. The built-in names are a separate list.

### Built-in names

`.DS_Store` and `Thumbs.db` are skipped at any depth, on top of the config list. `--no-default-blacklist` is the run that keeps them.

### Flags for one run

`--blacklist` replaces the blacklist from the file for that command. The built-in names still apply. `--whitelist` replaces the whitelist. Repeat either flag, or separate patterns with commas. Spaces around commas are dropped, and so are empty pieces.

```
bpm ingest /data/run42 --blacklist '*.txt,*.bak'
bpm ingest /data/run42 --blacklist '*.txt' --blacklist 'scratch/**'
bpm ingest /data/run42 --whitelist '*.fq.gz,*.bam'
bpm ingest /data/run42 --whitelist '*.fq.gz' --whitelist 'bams/**'
bpm scan /data/run42 --whitelist ''
bpm ingest /data/run42 --blacklist '' --no-default-blacklist
```

`--blacklist ''` is an empty extra blacklist for that run. `--whitelist ''` is an empty whitelist, so every path that survives the blacklist is eligible. A blacklisted path stays out when a whitelist pattern also matches it. `--no-default-blacklist` lifts `.DS_Store` and `Thumbs.db` only. A whitelist still has to match those names.

`bpm scan` with no target checks every location in the catalog, after the same filters. A path argument narrows the locations first, then the filters apply. `bpm scan /data/run42 --whitelist '*.fq.gz'` checks the fastq locations under that directory and leaves the others for a later scan.

### One directory

```
run42/
  S1.fq.gz
  notes.txt
  old.bak
  scratch/tmp.fq.gz
  keep/scratch/x.fq.gz
  .DS_Store
```

With `blacklist = ["*.txt", "scratch/**"]` in `~/.bpm/config.toml`:

| Command | Paths recorded or checked |
| --- | --- |
| `bpm ingest run42` | `S1.fq.gz`, `keep/scratch/x.fq.gz`, `old.bak` |
| `bpm ingest run42 --blacklist '*.bak'` | `S1.fq.gz`, `notes.txt`, `scratch/tmp.fq.gz`, `keep/scratch/x.fq.gz` |
| `bpm ingest run42 --whitelist '*.fq.gz'` | `S1.fq.gz`, `keep/scratch/x.fq.gz` |
| `bpm ingest run42 --no-default-blacklist` | `S1.fq.gz`, `keep/scratch/x.fq.gz`, `old.bak`, `.DS_Store` |

Each row is a fresh catalog. The second command replaces the file's blacklist, so `notes.txt` and `scratch/tmp.fq.gz` come in and `old.bak` stays out. `.DS_Store` is still a built-in name on that run. On the third command, `scratch/tmp.fq.gz` matches `*.fq.gz` and `scratch/**` still excludes it. `notes.txt` and `scratch/tmp.fq.gz` stay out on the fourth command because `--no-default-blacklist` only lifts the built-in names.

`bpm scan run42` uses the same four results to choose locations already in the catalog. Ingest is what adds a new file.

The same directory plus `S1.bam`, with both keys set:

```toml
blacklist = ["scratch/**"]
whitelist = ["*.fq.gz"]
```

| Command | Paths recorded or checked |
| --- | --- |
| `bpm ingest run42` | `S1.fq.gz`, `keep/scratch/x.fq.gz` |
| `bpm ingest run42 --whitelist '*.bam'` | `S1.bam` |
| `bpm ingest run42 --whitelist ''` | `S1.fq.gz`, `S1.bam`, `keep/scratch/x.fq.gz`, `notes.txt`, `old.bak` |
| `bpm ingest run42 --blacklist ''` | `S1.fq.gz`, `keep/scratch/x.fq.gz`, `scratch/tmp.fq.gz` |
| `bpm ingest run42 --no-default-blacklist` | `S1.fq.gz`, `keep/scratch/x.fq.gz` |
| `bpm ingest run42 --no-default-blacklist --whitelist ''` | `S1.fq.gz`, `S1.bam`, `keep/scratch/x.fq.gz`, `notes.txt`, `old.bak`, `.DS_Store` |

Each of these rows is also a fresh catalog. `scratch/tmp.fq.gz` matches `*.fq.gz` and stays out while `scratch/**` is in effect. `.DS_Store` stays out of the `--no-default-blacklist` row because the whitelist is still `*.fq.gz`. Clearing the whitelist on that same run is what lets the built-in name through. `notes.txt` and `old.bak` come in only when the whitelist is cleared, because neither name matches `*.fq.gz`.

## What ingest counts

Ingest prints one summary line on stdout. It does not list the paths. On stderr it names the directory and any whitelist or blacklist in effect. Every 100 files it rewrites a single count line, so a run of hundreds of thousands of files stays one line:

```
bpm: ingest /data/run42
bpm: whitelist *.png
bpm: blacklist .DS_Store, Thumbs.db
bpm: 200 files seen
```

That count uses a carriage return and a clear-to-end-of-line. When stderr is not a terminal, each update is a normal line instead, because a pipe cannot erase the previous one.

```
ingest 01a1…: 47 files seen, 44 new files, 47 new locations, 3 copies, 0 already recorded, 0 errors
```

| Count | What it is |
| --- | --- |
| files seen | Paths that passed the filters. This is the `find` count. |
| new files | File ids created. One id is one sequence of bytes, however many paths hold it. |
| new locations | Paths recorded on a file. |
| copies | Paths stored as another location because the bytes matched. `new files` plus `copies` is `new locations`. |
| already recorded | Paths that were already locations. Ingest leaves them alone. |
| errors | Paths that could not be recorded. They are also printed above the summary. |

The paths themselves stay in the catalog. `bpm query files` shows the locations of a file. A second ingest of a directory already in the catalog reports `already recorded` for every path and `0 copies`. A path whose fingerprint matched but whose bytes could not be read is a `possible duplicate`, printed on its own line, and is not merged. Errors are printed the same way. A broken symbolic link is not one of those errors: stderr gets a count (`bpm: 2 broken symbolic links`) and the run exits 0. The walk tried to follow it, and a missing target does not say whether it was a file or a directory.

## Linking files

`bpm link` attaches ingested files to entities. One file:

```
bpm link --to /CLL/WES-relapse --role document /data/irb/approval.pdf
```

A directory of FASTQs, creating each subject's Case, Sample, and one Raw Data node per file on the way:

```
bpm link --to '/CLL/WES-relapse/case[subject_id:{1}]?/sample?/raw_data[read:{2}]+' \
  --match '([A-Za-z0-9]+)_(R[12])\.fq\.gz' --role data --expect sample=2 /data/run42
```

`--to` is an address followed by typed steps, one level each. In a step, `[key:value]` selects the node and is written onto it if it is created. No suffix means the node must exist, `?` uses it or creates it, and `+` creates a new one. `{1}` and `{2}` are the pattern's groups. A mapping table (`--table samples.tsv --join 'barcode={1}'`) supplies placeholders when the file name does not carry the id.

Every run prints its plan first. `-n` stops there. A run that creates entities asks before it writes, or takes `--yes` without a terminal. A run writes everything or nothing, and it prints `link run <id>`. `bpm undo <id>` reverses that run, and `bpm undo --list` lists runs. `bpm unlink` takes the same `--to` and `--match`.

The full reference, with the checks and recipes for BAMs, FASTQs, and two assays from one sample, is [docs/guide/link.md](docs/guide/link.md).
