# BPM Product Requirements

**Status:** Draft 0.1
**Date:** 2026-09-29
**Product:** BPM, Biodata Project Manager
**Companion:** [Architecture overview](../architecture/overview.md)

This document is the product contract: who BPM is for, what each phase does, and the behavior a Core release has to demonstrate. The architecture overview records how that behavior is built. On behavior, this document wins. On mechanism, the architecture overview wins.

BPM is an open-source tool for biomedical file collections. It gives each file a durable identity, records where the bytes live, and attaches the file and its metadata to the program, project, participant, sample, assay, and analysis that give the file meaning. The bytes stay in the storage systems that already hold them.

A single local binary, `bpm`, ships first. A server, `bpmd`, follows. The same binary then logs into the server and operates on remote catalogs.

## 1. People

| Person | What they need |
| --- | --- |
| Lab data manager, bioinformatician | Index one initiative's files, attach sample metadata, see what moved or changed, and hand a colleague a manifest. This is the Core user. |
| Core facility staff | One catalog covering many projects, across more than one mounted filesystem. |
| Institutional data officer | Know what left the institution, under which policy, and what a withdrawal now affects. This is a Govern user. |
| Analyst or training pipeline | A manifest with stable file ids and checksums, and a tree that can be subset. |
| Collaborator receiving data | A release package shaped for the use they were approved for. This is a Govern recipient. |

Core assumes the person at the keyboard already has operating-system rights to the files. BPM-Core adds organization. BPM-Govern adds accounts, policy, and an account of what was released.

## 2. Phases

| Phase | Name | What ships |
| --- | --- | --- |
| 1 | BPM-Core | The `bpm` binary: catalogs, the entity tree, metadata, discovery, file identity, drift, query, import, provenance links, manifests, materialize, and a web UI. One operating-system user. No accounts. The v1 web UI is read-only. |
| 2 | BPM-Govern | `bpmd`: the same catalogs, plus authentication, roles, audit, policy enforcement, release packages, embargo, censoring and impact, deposition export, disbursement, clawback assistance, and AI-assisted policy review. |

Core is specified below to acceptance-test depth. Govern is specified as behavior the server phase has to meet. Govern's schema, API, and review-model choice are deliberately thin here and in the architecture overview.

### Scale

A Core catalog holds hundreds of thousands to millions of files, plus the metadata on the entities that own them. Hierarchical and metadata queries over a catalog of that size stay interactive on one workstation: on the order of a few seconds once the catalog is open. A benchmark catalog of 1,000,000 files and 5,000,000 metadata entries is part of the Core acceptance suite.

## 3. Shared concepts

| Term | Meaning |
| --- | --- |
| Catalog | One database. The unit of `bpm init`, of backup, and of later registration on a server. A person may keep several. |
| Entity | A node in the six-level tree. |
| File | A durable BPM identity for a sequence of bytes, independent of path. |
| Location | A place those bytes were seen, named by a storage backend and a URI. A backend may be a mounted filesystem or a remote object store such as S3. |
| Fingerprint | A fast checksum used to notice change and to suggest duplicates. It does not prove byte identity. |
| Digest | A full-file checksum tagged with its algorithm. This is what proves byte identity. |
| Drift | A location that disappeared, or bytes that no longer match the size, mtime, or digest BPM recorded. |
| Manifest | An immutable, reproducible listing of a selection from a catalog. |
| Release package | A Govern object: a policy-filtered manifest, an approver, and a record of who received it. |

Core and Govern share the catalog concepts. Release packages exist only in Govern.

Filesystem locations are the first implementation milestone. Remote object stores, including S3, are in Core's scope and land in a later milestone. The catalog records the same file identity, digest, and drift rules for both.

## 4. BPM-Core

### 4.1 Catalogs

A catalog is one database file.

`bpm init PATH` creates a catalog at `PATH`. `bpm init` with no path creates `~/.bpm/default.db`. Creating a catalog that already exists fails unless the operator passes `--force`, which replaces the catalog file. Replacing refuses a catalog that another process is writing, and a replacement that fails leaves the old catalog as it was. The catalog file is created so that only the current user can read and write it, and `~/.bpm` is created the same way. A command run while `~/.bpm` is readable by other users prints a warning and still runs.

Each catalog has its own id, assigned at init. Copying the file copies the id. Two catalogs never share rows.

Resolution order for every command that uses a catalog:

1. `--catalog PATH`
2. The `BPM_CATALOG` environment variable
3. `~/.bpm/default.db`

`bpm catalog` prints the catalog that this order resolves to. A missing default catalog produces an error that tells the operator to run `bpm init`.

The catalog stores metadata and pointers. It does not store file payloads. The only local state outside the catalog file is the small user config under `~/.bpm` (named catalogs, and later login material). That file does not select a catalog. There is no current-catalog setting.

Anyone who can read the catalog file can read every identifier and path in it, including participant ids. Core's protection is the file mode. Operators who need access control use Govern, and in that deployment only the service account can open the catalog files.

#### Integrity

`bpm` keeps the catalog consistent: every entity's parent exists, and every metadata row, link, location, and digest names an entity or file that exists. Another program writing the catalog file directly can break that. Before a command that walks or extends the entity tree, `bpm` checks that every entity's parent exists. If one does not, the command fails and tells the operator to run `bpm repair`.

`bpm repair` lists every row that breaks the catalog's integrity and exits non-zero when it finds any. `bpm repair --apply` removes those rows in one step. An entity whose parent is missing is removed with its descendants, their metadata, and their file links, as `bpm delete --cascade` would. File rows stay. An operator who would rather keep such an entity can move it with `bpm reparent`, or remove it with `bpm delete`, before applying the repair. Those two commands, and `bpm sql`, work on a catalog that fails the check. The [repair guide](../guide/repair.md) walks an operator through the choices.

### 4.2 Entity tree

Six entity types form a tree. Every entity except a Program has exactly one parent, and that parent is of the type directly above.

| Type | Slug | Parent |
| --- | --- | --- |
| Program | `program` | none |
| Project | `project` | Program |
| Case | `case` | Project |
| Sample | `sample` | Case |
| Raw Data | `raw_data` | Sample |
| Analysis | `analysis` | Raw Data |

A Case is the independent biological unit under a project: a participant, a cell line, a xenograft, or another unit the operator treats as one subject. A Sample is the biospecimen unit the operator chooses to attach raw data to. Analyte, portion, slide, and aliquot are metadata on that Sample, not extra types in the tree.

Creating an entity with the wrong parent type fails. A Program is the only type that can be created with no parent.

Every entity has a UUID, optional metadata, and zero or more linked files. Program and Project also have a name. Case, Sample, Raw Data, and Analysis do not.

A Program or Project name is required. It is unique among siblings, case-sensitive, and 1–256 characters, with no `/`, no `:`, no ASCII controls, and no leading or trailing whitespace. A Program's path is `/<name>`. A Project's path is `/<program name>/<project name>`, for example `/CLL/WES-relapse`. Renaming a Program or Project, or moving a Project under a new Program, changes those paths. Case, Sample, Raw Data, and Analysis are addressed by UUID, and by a metadata path as described in §4.4. Creating one returns that UUID. Their study identifiers, barcodes, run ids, and labels are metadata under whatever keys the operator chooses. `external_id` is one such key when an operator wants it: ordinary metadata, with no uniqueness rule, selectable by the same query and path rules as any other key.

The same person enrolled in two projects is two Case nodes. The tree does not link them. An operator who wants that correlation stores the same metadata value on both.

File ids are independent of entity UUIDs and of Program and Project names. Renaming a Program or Project leaves file ids unchanged.

An operator can delete an entity only when it has no children and no linked files. `--cascade` deletes the descendant entities and their file links. File rows that have no links left remain in the catalog as unlinked files. No delete command removes bytes from storage.

Whether `name`, or a required metadata pair, should also be required on Case, Sample, Raw Data, or Analysis is still open.

### 4.3 Where files belong

Files and metadata may sit on any entity. The placement below is the practice queries and release rules are written against. Core accepts other placements.

| Kind of thing | Attach it to |
| --- | --- |
| Instrument output for one sample (FASTQ pair, SVS, CEL) | A Raw Data node under that Sample |
| Pipeline output from that one raw input (BAM, gVCF, segmentation mask) | An Analysis node under that Raw Data |
| Cohort product (joint VCF, merged matrix) | A shared ancestor of the inputs. For a cohort VCF, the Project |
| Paperwork (consent scan, IRB letter, samplesheet) | The Case or the Project. The role is the operator's label, for example `document` |
| Program-level report | The Program |

A multiplexed instrument run is one Raw Data node per sample, sharing a `batch_id`. The run is not a parent of those nodes.

An artifact that combines data from several nodes is attached to a shared ancestor of those nodes. A cohort VCF built from many samples is a file on the Project. It is not an Analysis node under one of those samples, because an Analysis has a single Raw Data parent. Which input files went into that VCF can be recorded separately, as links from the output file to those inputs (§4.7).

A specimen and a slide that both need to be first-class nodes are sibling Samples under the same Case. The conventional metadata key `source_sample` holds the other sample's UUID. Core does not enforce that string as a foreign key.

### 4.4 Metadata

Metadata is a string key and a string value. Any key is allowed. Each object has at most one value for a given key; setting it again replaces the value. A list is stored as one value the operator formats (for example a JSON array). Core does not interpret list syntax.

Keys and values are non-empty and must not contain `:` or `/`. Those two characters are reserved so a path can name a pair. A calendar date is written `YYYY-MM-DD`.

An operator can query entities or files by a key (`subject_id:`), by a value (`:CLL-001`), or by a pair (`subject_id:CLL-001`). Repeatable filters are a conjunction.

The same three forms are path segments after a Program path, a Project path, or an entity UUID. `/CLL/WES-relapse/ext_id:CLL-001` selects the descendants of that Project whose metadata has `ext_id` equal to `CLL-001`. A later segment continues from those matches: `/CLL/WES-relapse/ext_id:CLL-001/sample_kind:aliquot`. A command that needs one entity requires the path to match exactly one. A query returns every match.

Metadata on an entity stays on that entity. Children do not receive a copy. A query for everything under a Case finds the children; it does not pretend the Case's consent value was written onto each Sample.

Govern currently gives special meaning to two keys when it enforces policy. Core stores those keys like any other metadata and returns them in query and manifest results. As Govern is developed, it may define further canonical keys with their own meaning.

| Key | Usual place | Govern reading |
| --- | --- | --- |
| `consent` | Case, or an ancestor | Allow-list match, described in Govern |
| `embargo_until` | Any entity | ISO 8601 date; withheld through that date |

The keys below are examples, so two catalogs have a shared vocabulary to start from. Core stores them like any other key and can filter on them. A later release may give some of these keys special meaning, including in Core, and may require some of them in some circumstances. Other keys and other values remain valid.

| Key | Usual place | Suggested values |
| --- | --- | --- |
| `external_id` | Any entity | Site barcode or subject id |
| `sample_kind` | Sample | `specimen`, `portion`, `analyte`, `slide`, `aliquot`, `pool`, `other` |
| `analyte` | Sample | `DNA`, `RNA`, `protein`, `cells`, `tissue`, `other` |
| `batch_id` | Raw Data | Instrument run id shared by sibling Raw Data nodes |
| `assay` | Raw Data | `WES`, `WGS`, `RNA-seq`, `scRNA-seq`, `panel`, `methylation`, `imaging`, `other` |
| `platform` | Raw Data | Instrument or kit name |
| `pipeline` | Analysis | Pipeline name |
| `pipeline_version` | Analysis | Version or commit |
| `reference` | Analysis | Reference genome or panel |
| `source_sample` | Sample | UUID of a related Sample |
| `description` | Any | Free text |
| `format` | File | `fastq`, `bam`, `vcf`, `svs`, `pdf`, other |
| `accession` | File or entity | Public repository accession, once one exists |

File metadata uses the same key-value rules and is stored on the file, separate from entity metadata.

### 4.5 Files, locations, and digests

A file row is created the first time a scan sees a new sequence of bytes that BPM cannot yet tie to an existing digest. The file id is a UUID. It survives renames, moves, and additional copies.

A file has:

- at most one current fingerprint, stored with the scheme that produced it. It may be absent when the bytes were not read. A later scheme does not invalidate the file id or the rows already stored
- zero or more digests, each tagged with its algorithm. The current bytes have at most one digest per algorithm. Older digests remain in a generation history
- one or more locations
- zero or more links to entities

The digest Core computes by reading bytes is BLAKE3, recorded `blake3:<hex>`. An operator can also request MD5 when a repository requires it. BLAKE3 and MD5 are two checksums of the same bytes. A checksum supplied by a storage backend is stored under that backend's algorithm and is trusted in place of a download, as §4.6 describes. Acknowledging a change moves every digest of the old bytes into the generation history, and the digests observed for the new bytes become current. A fingerprint is never written into a manifest content id.

A location is one place those bytes were seen. The usual case is a single location, recorded by the ingest that created the file. Another location is added only when a later ingest finds a path, or later an object-store URI, whose full digest matches this file. Until a digest exists, a second path is a new file id and a possible-duplicate report. The operator does not create locations by hand. Each location has its own presence, so one copy can be missing while another is readable, and drift is reported per location. Linking an entity links the file id, and every present location is that file. Byte summaries count the file once. Materialize reads any location that is present. After a move, the old location stays on the file as missing until the operator removes that location. Removing a location leaves the file and its other locations in place.

Zero links is the normal state of a file an ingest has not yet attached. One link attaches the file to one entity. Further links attach the same file id to other entities without a second copy of the bytes. That is how one control or reference file is associated with several samples, and how one consent document is associated with two cases. Repeating `bpm link` adds a link. A query for files under an entity returns the file when any link points there. Removing one link leaves the others. The file is unlinked when the last link is removed. In a Govern release, the file is withheld where the release reaches it only through excluded entities. A link on an included entity still includes the file. A document linked to both a Project and a Case remains available to the Project when that Case is excluded.

A link has a role. The role is an open string chosen by the operator. Core gives no role special behavior, and no role name is reserved. Example: a companion such as a BAI is its own file, linked to the same entity as the BAM, and might be assigned role `index`. A primary flag is not part of the command until we define what it changes.

### 4.6 Ingest and scan

Ingest and scan are different commands. Ingest adds files to the catalog. Scan checks files already in the catalog: it computes digests and reports drift. Inclusion and exclusion rules for a walk, such as which names to skip and how to treat symlinks, are specified in the architecture overview.

The quick fingerprint and the full digest are computed at different times. The fingerprint scheme can change later. Existing rows keep the scheme they were written with, and the catalog stays valid without recomputing them. The sample layout of the first scheme is in the architecture overview.

- **Fingerprint, on ingest of a new file whose bytes are read.** Example: `bpm ingest /data/run42` stores a fingerprint for each new FASTQ along with its size and mtime. A second ingest of a path the catalog already has does not recompute it. For a large file the fingerprint is a sample of the bytes, not a read of the whole file.
- **Fingerprint again, when scan reads the bytes and sees a new size or mtime.** The stored fingerprint is left alone while the size and mtime still match, and while the bytes are not read. It describes the bytes of the current digest. A copy whose bytes no longer match that digest does not replace it. Acknowledge does.
- **BLAKE3, when scan reads a selected location.** Example: `bpm scan /data/run42` reads those files in full and stores `blake3:<hex>`. That is the first time a typical local file gets a complete digest. A later scan that reads the file again either confirms the stored digest or marks `digest_mismatch`.
- **BLAKE3, during ingest, only to test a duplicate whose bytes are read.** A new path with the same size and the same fingerprint scheme and value as a file already in the catalog is hashed in full. If that existing file has no BLAKE3 yet, ingest hashes its bytes too. Matching digests add a location. On a local filesystem this is the only ingest that reads a whole file.
- **A backend checksum, when the backend publishes one.** Object stores such as S3 can charge for a download and also store a checksum. Ingest and scan trust that checksum and do not download the object. An explicit flag on either command forces the download and a locally computed BLAKE3. The flag spelling, and the configurable denylist and glob whitelist, are in the architecture overview.

`bpm ingest PATH` walks a directory and records regular files that are not already in the catalog. Example: `bpm ingest /data/run42` creates a file row for each new FASTQ, with size, mtime, fingerprint, and that path as its location. The files are unlinked. A path the process cannot stat or read is reported and gets no fingerprint. The ingest commits as it goes. An interrupted ingest leaves a consistent prefix, and running it again finishes the work.

Ingest does not check files the catalog already knows. Example: `/data/run42/S1_R1.fq.gz` is already ingested, and someone appends to it overnight. A second `bpm ingest /data/run42` does not update that file's size, mtime, fingerprint, or digest, and it does not mark anything missing. Finding that change is scan's job.

Ingest consults existing file rows, linked or not, only when a newly seen path looks like a duplicate of one of them: same size and same fingerprint scheme and value, or the same size and a matching backend checksum. It then compares digests, reading bytes only when no trusted checksum is available. If the existing file has no BLAKE3 yet and the bytes are being read, ingest hashes that file too. Matching digests make the new path another location of the existing file id. Different digests leave two file ids. If one of the two cannot be hashed, ingest reports a possible duplicate and does not merge. Example: `S1_R1.fq.gz` is ingested from `/data/run42` and later copied to `/archive/run42`. `bpm ingest /archive/run42` adds `/archive/run42/S1_R1.fq.gz` as a second location of the same file id. The old path is unchanged by that ingest. Fingerprint equality alone never merges two file ids.

`bpm scan` compares each selected location to the catalog. On a local filesystem it computes a BLAKE3. On an object store that publishes a checksum it uses that checksum, and downloads the object only when the explicit flag is set. There is no separate hash command and no `--hash` flag. Further scan parameters are TBD. With no arguments, scan covers every location in the catalog. A storage backend limits the scan to locations on that backend. Example: a file has one location on `posix` and one on `s3`. `bpm scan s3` drift-checks only the `s3` location. A path with no backend means the local filesystem backend. Example: `bpm scan /data/run42` checks ingested locations under that path on the local filesystem, and does not add files that were never ingested.

Drift states an operator can filter on:

| State | Meaning |
| --- | --- |
| `missing` | The location was absent when scan looked for it |
| `stat_changed` | Size or mtime differs from the last observation |
| `digest_mismatch` | A digest just compared differs from the current digest of that algorithm |
| `ok` | Present, stat unchanged, and the computed digest matches the current digest, or no digest was stored yet and the new one is recorded |

Example: after `bpm ingest /data/run42`, truncating `S1_R1.fq.gz` and running `bpm scan /data/run42` marks that location `stat_changed`. It stays `stat_changed` on later scans until acknowledge accepts it. Scanning it again after the bytes differ from a stored BLAKE3 marks `digest_mismatch`. `bpm acknowledge` on that file accepts the bytes now at its locations as the next generation: previous digests stay in the history, the new digest becomes current, and the drift state returns to `ok`. Manifests already issued keep the digest they were created with.

`bpm acknowledge` takes exactly one file, named by its file id or by the path of one of its locations. It never accepts drift for more than one file per command. There is no `--all`, and a directory, a backend, or a glob is not accepted. Drift is evidence that bytes changed, and accepting it is a decision about one file. Example: a scan of `/data/run42` reports ten `digest_mismatch` locations. Accepting them is ten `bpm acknowledge` commands, one per file. Acknowledge reads the bytes at every present location of that file. If those locations do not hold the same bytes, it refuses and changes nothing, and the operator restores or removes the wrong location first. A missing location stays missing.

A move is an ingest of the new path plus a scan of the old one. Example: a hashed file leaves `/data/run42` and appears at `/archive/run42`. Ingest of the archive adds the new location on the same file id. Scan of `/data/run42` marks the old location `missing`. The old location stays on the file until the operator removes it.

### 4.7 Provenance

The behavior below is the intended model. How derived-from edges and the command history are captured, recorded, and stored is not settled. Concrete implementation is deferred to a later milestone. The parent tree in §4.2 is part of the earlier work and is not deferred with this section.

Two independent structures:

1. **The tree.** Parent links answer "what is under this Case?" and, in Govern, carry policy from ancestor to descendant.
2. **Derived-from edges between files.** An edge says the output file was produced from the input file. Edges are many-to-many. They are how a cohort VCF on a Project points at the per-sample BAMs under Cases, and how a pool points at its inputs.

`bpm derive OUTPUT INPUT` records an edge. Recording an edge does not move either file in the tree.

Core can walk the edges in both directions:

- **Forward.** Files derived, directly or through a chain, from the files under a given entity.
- **Backward.** Files an output was derived from, and the entities those inputs are linked to.

Core presents these walks as query results. It does not hide files because of them.

Command history is a third, weaker record: an append-only log of CLI and web actions (time in UTC, operating-system user, working directory, catalog id, action text). It is an operator notebook. It is not a compliance audit log. Govern audit is specified separately.

### 4.8 Query

`bpm query` is the supported query interface. It is what the web UI uses as well. The specific flags below are a working sketch and are subject to further revision.

| Query | Returns |
| --- | --- |
| `entities --under TARGET --type TYPE --where SELECTOR` | Matching entities. `TARGET` is a Program or Project path, an entity UUID, or a metadata path from §4.4. `SELECTOR` is `key:value`, `key:` (that key is present), or `:value` (that value is present on any key). Repeatable `--where` is a conjunction. `--under` includes the entity and its descendants. |
| `files --under TARGET --role ROLE --format FORMAT --where SELECTOR` | Files linked to those entities, plus files linked directly, with digest, size, locations, drift, and link role. `SELECTOR` filters file metadata with the same three forms as an entity query. |
| `files --unlinked` | Files with no entity link. |
| `files --drift STATE` | Files with a location in that drift state. Repeatable. |
| `files --digest blake3:HEX` | The file with that digest. Any stored algorithm uses the same `algorithm:hex` form. |
| `impact --under TARGET` | Files outside the subtree that are forward-reachable by derived-from from files inside it, and the edges used. `TARGET` is a Program or Project path, an entity UUID, or a metadata path from §4.4. |
| `lineage FILE` | Derived-from ancestors and descendants of one file, with the entities those files are linked to. |
| `summary` | Counts and bytes by entity type, by `sample_kind`, by `assay`, by storage backend; plus unlinked count and drift counts. |

Default output is a readable table. `--format csv` and `--format json` are stable enough for scripts. Dates are UTC ISO 8601. Program and Project rows include their path. Other entity rows include their UUID.

`bpm sql` runs a statement against a local catalog. The default is read-only. `--write` is required for a statement that changes rows. Recording that write in the command history waits for the §4.7 milestone. A remote Govern catalog does not accept `bpm sql` in the first server release; remote clients use `bpm query`, which goes through policy filtering.

### 4.9 Import

**UNSTABLE: subject to revision and change.**

`bpm import` creates and updates entities, metadata, file links, and derived-from edges. Re-importing the same file is an update of metadata and links, not a second tree.

Two input shapes:

**Wide TSV** for entities. Required columns: `type` and `parent`. `parent` is a Program or Project path, or the UUID of the parent, and it is empty for a Program. Program and Project rows also require `name`. Further columns become metadata keys.

**JSON document** for anything the TSV cannot say cleanly: file links (by current path or by digest), file metadata, and derived-from pairs.

A path in an import that has not been ingested fails that row with a message, and the rest of the import still commits. The operator ingests first, then imports links.

### 4.10 Manifests and materialize

**UNSTABLE / TO BE REVIEWED.**

`bpm manifest --under PATH` (with the same `--type` and `--where` filters as query) writes an immutable manifest of the matching entities and the files linked under them. Policy keys are included as metadata on the entities that carry them, and every matching row is listed. Unlinked files are included when the operator passes `--unlinked`.

The manifest has two ids:

| Id | Hashes | Use |
| --- | --- | --- |
| `content_id` | Entity paths and types, metadata, file ids, digests, link roles, derived-from edges among the included files | The portable identity of the selection. Locations are excluded so the id survives a storage move. |
| `snapshot_id` | The content body plus locations and drift states at creation | An operational snapshot of one site at one time. |

Canonical JSON is UTF-8, LF, sorted keys, no insignificant whitespace, entities in tree order (Program and Project by path, then remaining entities by parent and UUID), files in id order. Creating a manifest whose canonical content body matches an existing manifest returns that manifest instead of writing a second row. The content body stays as issued: a later rename or a storage move leaves it unchanged.

When the content matches and the locations have since changed, the command still returns the stored `content_id` and also prints the snapshot hash of the catalog as it stands now. `--refresh-snapshot` writes that new snapshot onto the stored row. It is the only edit a manifest row accepts, and it leaves the content body and `content_id` alone.

Files without a BLAKE3 are included and marked `unhashed`. `bpm manifest --require-hash` fails if any included file lacks BLAKE3. A checksum that came from the storage backend does not satisfy that flag.

`bpm materialize MANIFEST DEST` stages the files named in a manifest into a destination directory and writes the manifest JSON beside them.

- The default is a byte copy, read from any present location of the file.
- `--link hard` and `--link symlink` are explicit.
- A copy remains a separate byte range from the archive, and it remains valid after the source path moves. Copy is the default for that reason.
- Materialize leaves source bytes and source mtimes unchanged.
- If the destination already contains a file that would be overwritten, materialize fails unless `--force` is given.
- When a file's locations are all missing, bytes already copied by that run stay in the destination, and a materialize report beside them lists the missing file ids with `partial: true`. The catalog manifest's content body stays as issued. The command exits non-zero. The operator re-runs after repairing locations.

### 4.11 Local web UI

`bpm serve` starts a web UI. The default is `127.0.0.1` port `3000`. Example: `bpm serve` answers at `http://127.0.0.1:3000`.

Host, port, and token each have a flag and an environment variable. The flag wins when both are set.

| Setting | Flag | Environment variable | Default |
| --- | --- | --- | --- |
| Host | `--host` | `BPM_HOST` | `127.0.0.1` |
| Port | `--port` | `BPM_PORT` | `3000` |
| Token | `--token` | `BPM_TOKEN` | none |

The token is a shared secret for interactive login. When one is configured, the UI asks the operator to type it before showing the catalog. A wrong token is refused. The token is not written into the catalog. On the default host the token is optional. If `--host` or `BPM_HOST` is set, a token is required and `bpm serve` exits with an error when neither `--token` nor `BPM_TOKEN` is set. Example: `BPM_HOST=0.0.0.0 BPM_TOKEN=lab BPM_PORT=3000 bpm serve` listens on every interface and will not serve pages until the operator enters `lab`.

Core v1 is a read-only web interface. The browser shows the catalog and does not change it. Writes stay on the CLI. Example: a link created with `bpm link` appears on the file page after refresh. The page has no control that edits metadata, acknowledges drift, ingests, scans, deletes, or builds a manifest.

Core v2 is a read-write web interface. What it can change, and the UI for those changes, is TBD.

The v1 pages are:

| Page | What the operator can do |
| --- | --- |
| Home | Summary counts, drift, unlinked files, recent ingests and scans |
| Tree | Expand the entity tree, open an entity |
| Entity | Read metadata, see children and linked files, follow the breadcrumb |
| File | Id, fingerprint, digests, locations, drift, and links. Derived-from waits for the §4.7 milestone |
| Search | The same filters as `bpm query`. Those flags are still a sketch (§4.8) |

### 4.12 Policy metadata in Core

Core stores `consent`, `embargo_until`, and any other policy note the operator wants. Query, manifest, materialize, and export return matching rows whether or not those keys are present. Filtering, embargo, redaction, and clawback are Govern behavior.

### 4.13 Command vocabulary

Rows with an empty Unstable cell are the Core command contract. A `*` means the command is under consideration and is not finalized, in this section or in the section cited. Flag spelling for an unmarked command is fixed when that command is implemented. `bpm scan --md5` requests an MD5 digest as a second pass. The download flag for object stores is deferred until that stage. Other scan flags are still TBD and are not part of the contract yet. `bpm link` takes an entity and a role string. It does not take a primary flag. That idea is undefined until we give it behavior.

The token typed into `bpm serve` is not `bpm login`. `bpm login` is the future client for a Govern server.

| Command | Behavior | Unstable |
| --- | --- | --- |
| `bpm init` | Create a catalog | |
| `bpm catalog` | Show the resolved catalog | |
| `bpm create TYPE` | Create an entity | |
| `bpm rename` | Rename a Program or Project | |
| `bpm reparent` | Move an entity under a new legal parent | |
| `bpm delete` | Delete an entity, a link, a location, or a file row. Never deletes bytes | |
| `bpm meta set / get / unset` | Edit metadata on an entity or a file | |
| `bpm ingest` | Add files from a path. Does not recheck files already in the catalog, except when a new path is a duplicate of one | |
| `bpm scan` | Report drift for locations already in the catalog. A local filesystem read stores BLAKE3. `--md5` also stores MD5. An object store that publishes a checksum is trusted unless a download is requested. No arguments scans every location. A backend or a path narrows that set | |
| `bpm link` / `bpm unlink` | Attach or detach a file and an entity. The role is an open string | |
| `bpm acknowledge` | Accept drifted bytes of one file as a new generation. Takes one file id or one location path. Never more than one file | |
| `bpm sql` | Local SQL, read-only unless `--write` | |
| `bpm repair` | List rows that break the catalog's integrity. `--apply` removes them | |
| `bpm serve` | Web UI. Default `127.0.0.1:3000`. `--host` / `BPM_HOST`, `--port` / `BPM_PORT`, `--token` / `BPM_TOKEN`. Token required when the host is set. v1 is read-only | |
| `bpm query` | Structured query. Flags are a sketch (§4.8) | * |
| `bpm import` | TSV or JSON ingest (§4.9) | * |
| `bpm manifest` | Create or fetch a manifest (§4.10) | * |
| `bpm materialize` | Stage a manifest onto a destination (§4.10) | * |
| `bpm derive` | Add or remove a derived-from edge (§4.7) | * |
| `bpm history` | Show the command notebook (§4.7) | * |
| `bpm login` / `bpm logout` / `bpm use` | Govern client. In Core these fail with a short message that no server is configured | * |

### 4.14 Worked example

A data manager keeps the CLL grant in its own catalog.

```
bpm init ~/work/cll/bpm.db
bpm create program --name CLL
bpm create project --parent /CLL --name WES-relapse
bpm create case --parent /CLL/WES-relapse
# prints the Case UUID
bpm meta set <case-uuid> subject_id CLL-001
bpm meta set <case-uuid> consent GRU
bpm meta set <case-uuid> embargo_until 2027-01-01
bpm create sample --parent <case-uuid>
# prints the Sample UUID
bpm meta set <sample-uuid> sample_kind aliquot
bpm meta set <sample-uuid> analyte DNA
bpm meta set <sample-uuid> barcode ALQ-DNA
bpm create raw_data --parent <sample-uuid>
# prints the Raw Data UUID
bpm meta set <raw-uuid> assay WES
bpm meta set <raw-uuid> batch_id RUN42
```

They run `bpm ingest` on the run directory. The FASTQs appear as unlinked files. They link `S1_R1.fq.gz` and `S1_R2.fq.gz` to the Raw Data UUID with role `data`. A slide for the same participant is a second Sample under the same Case, with `sample_kind=slide` and `source_sample` set to the first Sample's UUID. The SVS is linked to a Raw Data node under that slide Sample.

While that Case is the only entity under the Project with that subject id, its address is `/CLL/WES-relapse/subject_id:CLL-001`.

A joint VCF is linked to `/CLL/WES-relapse` with role `data`, because it combines data from more than one Sample. Which BAMs produced it, and a query that walks that relationship, wait for §4.7.

A manifest of `/CLL/WES-relapse` is the §4.10 sketch, which is not finalized. In that sketch the Case, the embargo key, and the joint VCF are included, and the embargo does not remove the Case.

### 4.15 Acceptance scenarios

Scenarios below describe Core behavior. A scenario marked deferred or unstable becomes a test when that milestone is built. It is not part of the settled Core contract. Where a scenario names a `bpm query` flag, the behavior is the §4.4 selectors and the spelling is the §4.8 sketch.

1. **Init.** `bpm init PATH` creates a catalog readable and writable only by the current user. A second init without `--force` fails and leaves the file untouched. `bpm init` with no arguments creates `~/.bpm/default.db`.
2. **Resolution.** With no flag and no `BPM_CATALOG` environment variable, a command uses `~/.bpm/default.db`. `BPM_CATALOG` overrides the default. `--catalog` overrides both. A `bpm.db` sitting in the current directory is not selected on its own.
3. **Chain.** An operator can create Program → Project → Case → Sample → Raw Data → Analysis. Program and Project are addressed by path. The other four are addressed by the UUID returned at creation, and by a metadata path that matches exactly one entity, such as `/CLL/WES-relapse/ext_id:CLL-001`.
4. **Illegal parent.** Creating a Sample under a Program fails. Creating a second parent for a Case fails. The catalog is unchanged by the failed command.
5. **Names.** Two Projects under one Program cannot share a name. The same Project name under two Programs is allowed. A Program or Project name that contains `/`, `:`, or surrounding whitespace is rejected. Case, Sample, Raw Data, and Analysis accept no name.
6. **Rename and reparent.** Renaming a Program changes the paths of its Projects and leaves Case UUIDs unchanged. Reparenting a Case onto another Project in the same catalog succeeds. Reparenting a Case onto a Sample fails. When §4.10 is finalized, a manifest issued before the rename still shows the Program and Project paths it stored.
7. **Metadata.** Set, replace, and unset work for an arbitrary key. A second set replaces the value. The value is returned unchanged by query. A key or value containing `:` or `/` is rejected. A selector for a key (`subject_id:`), a value (`:CLL-001`), and a pair (`subject_id:CLL-001`) each returns the Case. The path `/CLL/WES-relapse/subject_id:CLL-001` resolves to that Case when it is the only match, and fails when two entities under that Project share the pair. When §4.10 is finalized, a manifest returns the same metadata.
8. **Conventional keys.** `sample_kind`, `assay`, `consent`, and `embargo_until` round-trip and are usable in `--where`. An unknown `sample_kind` is stored.
9. **Isolation of metadata.** A `consent` value on a Case is returned on the Case. It is not copied onto the Sample. `--under` the Case still returns the Sample.
10. **Ingest.** `bpm ingest` of a directory of regular files creates unlinked file rows with size, mtime, fingerprint, and a location. No file payload is written into the catalog.
11. **Second ingest.** An immediate second ingest of the same directory creates no new file ids and does not change size, mtime, fingerprint, or digest.
12. **Ingest ignores existing drift.** Changing a file's size on disk and ingesting its directory again leaves the catalog's size, mtime, and digest as they were. `bpm scan` of that path then marks `stat_changed`.
13. **Duplicate ingest.** After `bpm ingest` of a file and `bpm scan` of it, ingesting a second path with the same bytes adds that path as a location of the same file id. The file may be linked or unlinked. The same second path, ingested before any BLAKE3 exists, causes ingest to hash both copies and still results in one file id when the digests match.
14. **Unreadable path.** A file the process cannot read is reported on the ingest. It does not receive a fingerprint.
15. **Stat drift.** Changing the size of an ingested file and running `bpm scan` on its path marks `stat_changed` and keeps the file id. A backend argument scans only locations on that backend. A file with one location on the selected backend and another elsewhere is checked only on the selected backend. Object-store backends are the later milestone in §3, and this half of the scenario runs when a second backend exists.
16. **Digest drift and acknowledge.** `bpm scan` of a local file stores BLAKE3. Modifying the file and scanning again marks `digest_mismatch`. `bpm acknowledge` sets the new digest current and retains the previous digest in the generation history. `bpm acknowledge` of a directory fails and changes nothing. Acknowledging one of two drifted files leaves the other `digest_mismatch`.
17. **Move with a digest.** A file that scan has hashed is moved. Ingest of the new path keeps the file id and adds the new location. Scan of the old path marks the old location `missing`. The old location remains until the operator removes it.
18. **Move without a readable source.** The same move, when the old bytes are already gone and no BLAKE3 was stored, yields a missing location after scan of the old path, a new file id from ingest of the new path, and a possible-duplicate report. The two ids are not merged.
19. **Two copies.** Two paths with identical bytes that are both ingested are one file id with two locations once their BLAKE3 values have been compared.
20. **Link.** Linking a file to a Raw Data node with role `data` makes `bpm query files --under` the Case return it. A second link to another entity succeeds. Unlinking one leaves the other.
21. **Unlink and delete.** Unlinking leaves the file row. Deleting a Program that still has a Project fails. `bpm delete --cascade` on that Program removes descendant entities and links, leaves previously linked files as unlinked rows, and leaves source bytes on disk.
22. **Provenance walks. Deferred (§4.7).** A Project-level file with derived-from edges to two BAMs under two Cases is returned by `impact --under` each Case. `lineage` on the Project file returns both BAMs and both Cases.
23. **Import. Unstable (§4.9).** A wide TSV creates the tree and metadata. A JSON import links ingested paths. A link that names a path not yet ingested fails that row and keeps the rows that succeeded.
24. **Manifest identity. Unstable (§4.10).** Two manifest generations with no intervening edit return the same `content_id` and the same stored `snapshot_id`. After a location change that leaves bytes, digests, and metadata the same, a new generation returns the original `content_id`, prints a snapshot hash of the current locations, and leaves the stored snapshot in place until `--refresh-snapshot`. `--require-hash` fails while any included file lacks BLAKE3, and succeeds after a scan that read the bytes has stored that digest.
25. **Policy is data.** A Case with `embargo_until` in the future is still returned by query. When §4.10 is finalized, that Case is still present in the manifest.
26. **Materialize. Unstable (§4.10).** Materialize copies bytes to the destination and writes the manifest JSON. Source bytes and source mtimes are unchanged. An existing destination collision fails the run unless `--force` is set. A missing location writes a materialize report with `partial: true` and the missing file ids, exits non-zero, and leaves the catalog manifest's content body unchanged.
27. **SQL hatch.** `bpm sql` of a `SELECT` succeeds. An `UPDATE` without `--write` fails and changes nothing. The same `UPDATE` with `--write` changes the row. Recording it in command history is part of the §4.7 milestone.
28. **History.** `bpm history` and the history rows are deferred with §4.7. When that milestone is built, a CLI metadata edit appends a history row, and history rows are not editable through the CLI or the v1 UI.
29. **Web.** `bpm serve` answers on `127.0.0.1:3000`. `BPM_PORT` and `--port` change the port. The tree and a file's drift state match the CLI against the same catalog. The page does not offer a write. `bpm serve --host 0.0.0.0` without a token exits with an error. With `BPM_TOKEN` or `--token` set, the catalog is shown only after the operator enters that token. The process does not listen on a non-loopback address unless `--host` or `BPM_HOST` asks it to.
30. **Two catalogs.** Entities created in catalog A are invisible to queries against catalog B.
31. **Benchmark.** A synthetic catalog of 1,000,000 files and 5,000,000 metadata entries runs `files --under` a Project and `entities --where` a metadata key. The harness records elapsed time. The target is interactive response, a few seconds, on one workstation.
32. **Repair.** A Project whose Program row was removed outside `bpm` makes `bpm query entities` fail with a message that names `bpm repair`, not a crash. `bpm repair` lists that Project and exits non-zero. `bpm repair --apply` removes it with its Cases and their metadata and links, keeps the file rows, and a second `bpm repair` reports the catalog consistent. `bpm reparent` can instead move the Project under an existing Program.

### 4.16 Core release slices

The scenarios in §4.15 that are not marked deferred or unstable are the contract. Implementation order is:

| Slice | Contents |
| --- | --- |
| A | Catalogs, entity tree, metadata, query, SQL hatch, repair |
| B | Ingest, scan, fingerprint, locations, drift, acknowledge, link |
| C | Import, manifests, materialize. Derived-from edges, impact, lineage, and command history are a later milestone (§4.7) |
| D | Read-only web UI (Core v1). Read-write UI is Core v2 and is TBD |

Slice A is already a useful notebook for entities. Slice B is the file indexer. The v1 web UI is part of the Core release, and it lands after the operations it displays. It does not add writes of its own.

## 5. BPM-Govern (UNSTABLE / TBD)

Govern is a server-side layer on the same catalogs. It does not introduce a second entity model. A catalog created by local `bpm` can be registered with `bpmd`.

### 5.1 Accounts and roles

Human users authenticate with an OIDC provider. BPM does not ship an identity provider. OIDC establishes who the person is. Role bindings live in BPM.

Machine clients use the OAuth client-credentials grant. A client id maps to a service account.

Roles are assigned per catalog. A user's permissions on a catalog are the union of their roles there.

| Role | Read | Edit catalog, ingest, and scan | Draft a release | Approve a release | Manage users and export audit |
| --- | --- | --- | --- | --- | --- |
| Viewer | yes | | | | |
| Curator | yes | yes | | | |
| Officer | yes | yes | yes | yes | |
| Admin | yes | | | | yes |
| Service | yes | yes | | | |

A Service account cannot approve a release. An Admin can export audit and manage bindings, and does not thereby gain approval rights. One person may hold Officer and Admin together.

Failed authorization attempts are audited. The web UI and the remote CLI enforce the same roles.

### 5.2 What the server operates on

`bpmd` serves the web UI and the API the `bpm` binary calls after `bpm login`. Remote clients use `bpm query`, `bpm manifest`, ingest, scan, link, and release commands. They do not get `bpm sql` in the first Govern release.

File bytes stay in the institution's storage. The server does not proxy BAM or SVS contents through the browser. Materialize and disbursement run where the storage is mounted, and they write a destination the operator names. The web UI shows paths, digests, and drift; it does not stream the research files.

### 5.3 Release packages

A release package is a Govern record with these states: `draft`, `in_review`, `approved`, `materialized`, `revoked`.

Building a draft:

1. The officer selects a population with the same filters as `bpm query`.
2. The officer names an allow-list of `consent` values (for example `GRU` and `not_human`) and an embargo cutoff, normally the current date.
3. BPM computes the included set, the excluded set, and the impacted set, with a reason on every excluded or impacted file.

Exclusion rules, applied to each entity and then to descendants:

- `consent` on the entity or any ancestor is missing, or is not in the release allow-list
- `embargo_until` on the entity or any ancestor is later than the cutoff
- `embargo_until` is present and is not a parseable ISO 8601 date

A missing consent excludes the node. Non-human data and "consent not applicable" are explicit values the officer can put on the allow-list, such as `not_human`.

A file is withheld where the release reaches it only through excluded entities. A link on an included entity still includes the file, so a document linked to both a Project and a Case remains available to the Project when that Case is excluded. Files outside the selection that are forward-reachable through derived-from from an excluded file are **impacted** and are withheld from the package. The decision report lists them separately from ordinary exclusions so an officer can see that a joint VCF on the Project was held because one input Case was excluded.

The manifest inside an approved package is a Core manifest built from the included set only. Its `content_id` covers files that passed the decision. Excluded and impacted files appear in the decision report, not in the manifest body.

Approval records the officer, the time, the allow-list, the cutoff, and the decision report. A Service account cannot transition a package to `approved`.

### 5.4 Disbursement and clawback assistance

Materializing an approved package records each recipient (name, institution, contact) and the destination description. That record is the basis for clawback assistance.

When consent is updated or an embargo is extended, an officer can ask what already-materialized packages contain a given Case's files or any file derived from them. BPM lists the packages, recipients, file ids, and content ids. The state of those packages can be set to `revoked`. BPM records the revocation and the reason. Collecting bytes back from a recipient is an operational procedure the officer performs; BPM keeps the list and the status notes the officer enters. It does not reach into an external system and delete copies.

The backward walk is available on any file, released or not: which Cases, through tree links and derived-from edges, are inside this file?

### 5.5 Deposition

An approved package can be exported as a deposition bundle: the manifest JSON, a CSV of file ids, digests, sizes, and formats, and the entity metadata table. Adapters that reshape this bundle into an SRA, dbGaP, EGA, or GDC submission are later work. The first Govern release produces the bundle.

### 5.6 Audit

Every authenticated action through `bpmd` appends an audit event: actor, role snapshot, action, object ids, before value, after value, time, client address, and success or failure. The application offers no update and no delete of audit events. An Admin can export them.

Command history inside a catalog remains the operator notebook from Core. Audit in the server operations database is the compliance record. Opening a catalog file with a raw database client bypasses audit. Production deployments run `bpmd` under an operating-system account that is the only account allowed to read the catalog files and the operations database.

### 5.7 AI-assisted policy review

An officer can submit a draft decision plus one or more policy documents (text stored on the Program, or text attached to the draft). BPM returns a concordance report:

- items where the decision matches the policy text
- items where they conflict
- items where the policy text is silent or the catalog lacks the metadata to tell

Each item cites the policy passage and the catalog objects involved. A person in the Officer role accepts or rejects the report. Accepting the report records it on the package. It does not approve the release. Approval remains a separate officer action.

The review client is replaceable. The first implementation targets the lab's standard model API, called from the server only. The browser never receives the API credential.

The grant's benchmark of review quality is part of Govern's research plan. This PRD requires the report shape and the human decision. It does not require a specific accuracy number yet.

### 5.8 Govern acceptance scenarios

These describe server behavior. They become tests when Govern is built.

1. A user who has not logged in receives no catalog data from `bpmd`.
2. A Viewer can query and cannot create an entity, scan, or approve a release.
3. A Service account can ingest, scan, and link, and cannot approve a release.
4. A release whose allow-list is `GRU` excludes a Case with no `consent`, a Case with `consent=DS`, and a Case with a future `embargo_until`, and includes a Case with `consent=GRU` whose embargo date has passed.
5. Descendants and directly linked files of an excluded Case are excluded with a reason that points at the Case.
6. A joint file on the Project, derived from a BAM under an excluded Case and a BAM under an included Case, is withheld as impacted and is absent from the package manifest.
7. An unparseable `embargo_until` excludes the node and says why.
8. Approving a package writes an audit event with the officer id and the decision's content id. A second client that only has the Viewer role cannot approve it.
9. After the package is materialized to a named recipient, changing that Case's consent and running clawback assistance lists the recipient and the file ids.
10. `bpm sql` against the remote catalog is rejected. `bpm query` against the same catalog honors the release rules when the query is made in a release context, and honors role checks always.
11. The concordance report cites policy text and catalog ids, and the release remains `draft` or `in_review` until an Officer approves it.
12. An Admin can export audit events and cannot edit them through the API.

## 6. Neighboring systems

BPM sits between storage and the people who use files. The work around it stays where it is.

| Work | Where it stays |
| --- | --- |
| Sequencing and image pipelines | Nextflow, Snakemake, CWL, or the pipeline a core already runs. BPM records the outputs and the derived-from edges the operator supplies. |
| Wet-lab tracking, reagents, instrument schedules | The laboratory information management system. BPM stores the identifiers the LIMS already assigned as metadata on the relevant entities. |
| Byte storage and replication | The filesystem, NAS, or object store. BPM records locations. |
| Public archives | SRA, dbGaP, EGA, GDC, and similar repositories. Govern prepares a bundle those submissions can be built from. |
| Clinical systems of record | The EHR or the study database. BPM is not the source of truth for clinical care. |
| Identity of staff | The institution's OIDC provider, once Govern exists. |

BPM-Lite, the R/Shiny proof of concept, is prior operational experience. This product replaces it. A migration path from BPM-Lite data is an open question, not a Core acceptance test.

## 7. Decisions recorded in this draft

These were settled before the draft, or chosen while writing it so the acceptance tests could be concrete.

| Topic | Decision |
| --- | --- |
| Documents | This PRD plus an architecture overview. Govern stays in this PRD at behavior depth. |
| Catalog scope | One database file per initiative. Several catalogs per person. Default `~/.bpm/default.db`. Resolution is `--catalog`, then the `BPM_CATALOG` environment variable, then that default. |
| Tree | Six types, parent fixed as the type above. Derived-from edges between files carry pools, multiplexed relationships, and joint analyses. |
| Entity names | `name` is required on Program and Project only, and forms their path. Case, Sample, Raw Data, and Analysis have no name and are addressed by UUID. Study identifiers are ordinary metadata. |
| Sample granularity | Analyte, portion, slide, and aliquot are metadata. Sibling Samples plus `source_sample` cover a specimen and its slide when both must be nodes. |
| Discovery | Scan first, link second. Unlinked files are kept. |
| Identity | Stable file id, plus tagged digests, plus locations. Fingerprint suggests, and only within one scheme. BLAKE3 confirms when bytes are read. A published backend checksum is trusted. |
| Ingest and scan | `bpm ingest` adds new paths and consults the catalog only for duplicates. `bpm scan` reports drift for locations already ingested, computing BLAKE3 when it reads bytes. `--md5` is an optional second pass. |
| License | Apache-2.0. |
| In-place change | The file id stays. Acknowledge starts a new digest generation and keeps the old one. |
| Policy in Core | Stored and returned. Query and manifest do not filter on it. |
| Manifest hash | `content_id` ignores locations. `snapshot_id` includes them. |
| Materialize | Core can stage a manifest. Default is a copy. Source bytes are never deleted by BPM. |
| SQL | Local, read-only unless `--write`. Absent from the first remote release. |
| Server database | Embedded, one file per catalog, for Core and for the first server. The engine is an architecture decision. Postgres is an exit described in the architecture overview. |
| Web in Core | v1 is a read-only UI on `127.0.0.1:3000`, after the operations it displays. `--host` / `BPM_HOST` requires `--token` / `BPM_TOKEN`. Read-write UI is v2 and is TBD. |
| Prior CLI sketch | The earlier `bpm_next` command list informed this vocabulary. It is not the specification. |

## 8. Open questions

Answers here would change this draft.

1. **Sample-to-sample lineage.** `source_sample` is a string with no foreign key. If officers need "this slide came from that aliquot" to drive censoring the way derived-from does for files, it has to become a real edge. Today only file edges drive impact.
2. **Format parsers.** Core v1 ingests metadata the operator supplies. It does not read FASTQ, BAM, VCF, or SVS headers. Parsers would improve augmentation and would widen Core's scope.
3. **BPM-Lite import.** No migration acceptance test until we know whether BPM-Lite data needs to land in a Core catalog.
4. **Consent allow-list strictness.** A missing `consent` excludes a node from a Govern release. Confirm that this is the institutional default, including for catalogs that mix human and non-human data.
