# Untracking files

**Status:** Draft
**Date:** 2026-10-27
**Companions:** [Product requirements](../product/prd.md) §4.6, [Architecture overview](../architecture/overview.md) §6, [Linking files](link.md)

`bpm untrack` is the inverse of `bpm track`. It removes catalog locations at or under a path, and removes a file row when its last location goes. It never reads, writes, or deletes bytes on disk.

```
bpm untrack /data/run42
bpm untrack -n /data/run42 /data/run43
bpm untrack --whitelist '*.bam' /data/run42
bpm untrack --cascade --yes /scratch/old
```

## What a path reaches

Each operand is resolved the way `track`, `scan`, and `link` resolve a path. It does not have to exist. The usual reason to untrack is that the directory was deleted or moved, and a scan has marked its locations `missing`.

A path that is a location reaches that location. Any other path reaches every location below it on the local filesystem, cut at a component boundary, so `/data/run1` does not reach `/data/run10`. An operand that reaches nothing fails the command, and nothing changes.

Untrack takes paths, not file ids. Untracking is about where a file is, and a file id names every place at once.

## Filters

Untrack applies the blacklist and the whitelist the way `scan` does: the lists in `~/.bpm/config.toml`, replaced by `--blacklist` or `--whitelist` for this run, plus the built-in blacklist unless `--no-default-blacklist`. A pattern is matched against the location's path relative to the operand. So the same flags undo a filtered track:

```
bpm track /data/run42 --whitelist '*.bam'
bpm untrack /data/run42 --whitelist '*.bam'
```

A filter only narrows what is removed. Locations it excludes stay, and the summary counts them. A config whitelist of `*.bam` therefore leaves every other location under the path in place. Pass `--whitelist ''` to untrack everything below it.

## What happens to a file

A file is decided on all of its locations, across every operand in the command.

| The file | Untrack |
| --- | --- |
| keeps a location that was not selected | removes the selected locations. The file id, digests, metadata, and links stay. |
| loses its last location and has no links | removes the file row with its locations, digests, and metadata |
| loses its last location and has links | refuses, unless `--cascade` also removes those links |

Some consequences:

- `S1.bam` tracked at `/data/run42` and `/archive/run42`. `bpm untrack /data/run42` removes one location. The file keeps its id and its archive copy.
- Both copies under `/data`. `bpm untrack /data` removes the file.
- A location marked `missing` or `stat_changed` is removed like any other. Untrack does not look at the disk.

Removing a location is easy to reverse. Tracking the path again finds the surviving copy by its bytes and adds the location back to the same file id. Removing a file row is not. Its id, digests, file metadata, and links are gone, and tracking the path again creates a new file id that nothing points to.

## Linked files

If any file would lose its last location while it still has links, untrack refuses the whole command and changes nothing. It lists each link of each such file, with the entity's type and id and the role:

```
bpm: linked	/data/run42/S1.bam	analysis 01a1…	data
bpm: linked	/data/run42/S1.bam.bai	analysis 01a1…	index
bpm: 2 linked file(s) would lose their last location; nothing was written. Unlink them first, or pass --cascade to remove their links
```

Unlinking first is the clearer choice, because it shows what is being detached. `--cascade` removes the links of files that untrack removes, and no others. Untrack never deletes an entity, even one left with no files.

A link run whose links were removed this way keeps its counts in `link_runs`. A later `bpm undo` of that run finds fewer links to remove and has no role to restore on a removed file. Neither is an error.

## The plan, and applying it

Untrack always prints the plan first:

```
120 locations selected, 4 excluded by filters
remove 20 locations from files with other copies, 100 files (3 with metadata), 0 links
```

`-n` stops there and writes nothing. `--detail` adds one line per removal:

```
location	/data/run42/S1.bam
file	01a1…	/data/run42/S2.bam
link	01a1…	analysis	01a1…	data
```

A `location` line is a location removed from a file that keeps another one. A `file` line is a location of a file row being removed; a file with two selected locations has two lines. A `link` line is a link removed by `--cascade`.

When the plan removes a file row, untrack asks `Apply? [y/N]` on a terminal. With no terminal it refuses, unless `--yes` is passed. A plan that only removes locations from files that keep another location does not ask.

The plan is made from a read snapshot, so nothing is locked while you answer. Untrack then plans again under the write lock and writes only if that plan is the one it showed. A link or a location another process committed in between stops the run with nothing written; run it again. `bpm serve` keeps reading throughout.

## What it prints

After applying, one line with the run id:

```
untrack 01a2…: 20 locations removed from files with other copies, 100 files removed, 0 links removed
```

A plan with nothing to remove, because the filters excluded every location, prints `nothing to untrack` and exits zero. The command exits non-zero when it refuses, when an operand reaches nothing, or when the prompt is declined.

Each applied untrack writes one row to `untrack_runs`: the arguments, the time, and the counts. It is a record, not an undo. There is nothing to restore without the rows it deleted.

## Not `delete`

`bpm delete` removes entities only. Before `untrack`, `delete --location` and `delete --file` removed catalog rows for files one at a time. `--location` could leave a file row with no location. `--file` removed every copy of a file. Untrack replaces both.
