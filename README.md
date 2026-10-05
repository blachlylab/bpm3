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
