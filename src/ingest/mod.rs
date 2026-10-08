//! `bpm ingest`, `bpm scan`, and `bpm acknowledge`: the filesystem side.
//!
//! Everything here that reads bytes runs with the catalog lock released. Each
//! batch of [`BATCH`] files is hashed first and then handed to the catalog,
//! which takes the write lock only to apply it.

pub mod filter;
pub mod hash;
pub mod walk;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::catalog::{
    AckObservation, AckOutcome, Candidate, Catalog, IngestEntry, Run, RunKind, ScanOutcome,
    ScanScope, ScanTarget, Seen,
};
use crate::error::Error;
use crate::model::{BACKENDS, Drift, FileRef, FileRow, Fingerprint, POSIX};
use filter::PathFilter;
use walk::{WalkItem, Walker};

/// Files per committed batch. Internal; a crash keeps every committed batch.
pub const BATCH: usize = 1000;

/// Threads that stat and fingerprint one ingest batch. On a network
/// filesystem each read waits on a round trip, so several in flight help.
const WORKERS: usize = 8;

/// How often the stderr count moves. A terminal rewrites one line; a pipe
/// gets a new line, because a carriage return would not erase the last one.
const PROGRESS_EVERY: u64 = 10;

/// How often a scan's line moves between those counts, so one large file does
/// not leave it still. A terminal redraws in place; a pipe gets fewer lines.
const SCAN_REDRAW: Duration = Duration::from_millis(500);
const SCAN_LOG_EVERY: Duration = Duration::from_secs(30);

#[derive(Debug, Default)]
pub struct IngestReport {
    pub run_id: Option<Uuid>,
    pub seen: u64,
    pub created: u64,
    pub located: u64,
    /// Paths whose bytes matched a file and were stored as another location of it.
    pub copies: u64,
    /// Paths that were already locations, or were seen twice in one batch.
    pub already: u64,
    /// Symlinks whose targets could not be resolved. Not an error: the run
    /// still succeeds, and stderr reports the count rather than the paths.
    pub broken_links: u64,
    pub errors: Vec<(String, String)>,
    /// A new path with the size and fingerprint of these files, which could not
    /// be hashed to confirm. Not merged.
    pub possible_duplicates: Vec<(String, Vec<String>)>,
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub run_id: Option<Uuid>,
    pub seen: u64,
    pub counts: BTreeMap<Drift, u64>,
    /// Every location found `missing`, `stat_changed`, or `digest_mismatch`.
    pub drifted: Vec<(Drift, String)>,
    pub errors: Vec<(String, String)>,
}

/// What `bpm scan` was asked to cover.
#[derive(Debug, Clone)]
pub enum ScanArg {
    All,
    Backend(String),
    Path(PathBuf),
}

impl ScanArg {
    /// A backend name with no `/` is a backend. Anything else is a path on the
    /// local filesystem; `./posix` names a directory called `posix`.
    pub fn parse(raw: Option<&str>) -> Self {
        match raw {
            None => Self::All,
            Some(name) if BACKENDS.contains(&name) => Self::Backend(name.to_string()),
            Some(path) => Self::Path(PathBuf::from(path)),
        }
    }
}

#[derive(Debug)]
pub enum AckReport {
    /// Nothing drifted and nothing new was asked for. The catalog is unchanged.
    NothingToDo(FileRow),
    Done {
        file: FileRow,
        outcome: AckOutcome,
    },
}

/// How the CLI names a file: a UUID is a file id, anything else a local path.
pub fn file_ref(raw: &str) -> Result<FileRef, Error> {
    if let Ok(id) = Uuid::parse_str(raw) {
        return Ok(FileRef::Id(id));
    }
    let path = walk::location_path(Path::new(raw))?;
    Ok(FileRef::Location {
        backend: POSIX.to_string(),
        uri: utf8(&path)?,
    })
}

/// The location path for `bpm delete --location`.
pub fn location_uri(raw: &str) -> Result<String, Error> {
    utf8(&walk::location_path(Path::new(raw))?)
}

pub fn ingest(
    catalog: &mut Catalog,
    root: &Path,
    filter: &PathFilter,
) -> Result<IngestReport, Error> {
    let root = fs::canonicalize(root)
        .map_err(|err| Error::Message(format!("{}: {err}", root.display())))?;
    if !root.is_dir() {
        return Err(Error::Message(format!(
            "{} is not a directory",
            root.display()
        )));
    }
    let root_uri = utf8(&root)?;
    announce("ingest", &root.display().to_string(), filter);
    let run = catalog.start_run(RunKind::Ingest, POSIX, &root_uri)?;
    let mut report = IngestReport {
        run_id: Some(run.id),
        ..IngestReport::default()
    };
    let result = ingest_walk(catalog, &run, &root, filter, &mut report);
    let (seen, created) = (report.seen, report.created);
    catalog.finish_run(run, result.is_ok(), seen, created)?;
    result.map(|()| report)
}

fn ingest_walk(
    catalog: &mut Catalog,
    run: &Run,
    root: &Path,
    filter: &PathFilter,
    report: &mut IngestReport,
) -> Result<(), Error> {
    let mut paths = Vec::with_capacity(BATCH);
    let mut errors = Vec::new();
    let mut progress = Progress::new();
    for item in Walker::new(root) {
        match item {
            WalkItem::Error { path, message } => {
                errors.push((path.to_string_lossy().into_owned(), message));
            }
            WalkItem::BrokenLink => report.broken_links += 1,
            WalkItem::File {
                path,
                rel,
                target_rel,
            } => {
                if !filter.allows(&rel) || target_rel.is_some_and(|target| !filter.allows(&target))
                {
                    continue;
                }
                report.seen += 1;
                paths.push(path);
                if paths.len() == BATCH {
                    ingest_batch(
                        catalog,
                        run,
                        std::mem::take(&mut paths),
                        std::mem::take(&mut errors),
                        report,
                        &mut progress,
                    )?;
                }
            }
        }
    }
    ingest_batch(catalog, run, paths, errors, report, &mut progress)
}

/// One path of a batch, before its bytes are read.
enum Probe {
    Unusable(String, String),
    Already,
    Fresh(PathBuf, String),
}

/// What stat and the fingerprint read found for a fresh path.
enum Read {
    NotAFile,
    StatFailed(String),
    Stat(fs::Metadata, io::Result<Fingerprint>),
}

/// Plan one batch with no lock held, then apply it. The stats and fingerprint
/// reads run on [`WORKERS`] threads; the plan is made in path order.
fn ingest_batch(
    catalog: &mut Catalog,
    run: &Run,
    paths: Vec<PathBuf>,
    mut errors: Vec<(String, String)>,
    report: &mut IngestReport,
    progress: &mut Progress,
) -> Result<(), Error> {
    let mut in_batch = HashSet::new();
    let mut probes = Vec::with_capacity(paths.len());
    for path in paths {
        let probe = match utf8(&path) {
            Err(err) => Probe::Unusable(path.to_string_lossy().into_owned(), err.to_string()),
            Ok(uri) if !in_batch.insert(uri.clone()) || catalog.location_exists(POSIX, &uri)? => {
                Probe::Already
            }
            Ok(uri) => Probe::Fresh(path, uri),
        };
        probes.push(probe);
    }
    let mut reads = parallel_map(&probes, |probe| match probe {
        Probe::Fresh(path, _) => Some(match fs::metadata(path) {
            Ok(meta) if meta.is_file() => {
                let print = hash::fingerprint(path);
                Read::Stat(meta, print)
            }
            Ok(_) => Read::NotAFile,
            Err(err) => Read::StatFailed(err.to_string()),
        }),
        _ => None,
    })
    .into_iter();

    let mut entries: Vec<IngestEntry> = Vec::new();
    // BLAKE3 of existing files hashed during this batch, by file id.
    let mut hashed: HashMap<Uuid, (String, String)> = HashMap::new();
    for probe in probes {
        progress.file();
        let read = reads.next().flatten();
        let (path, uri) = match probe {
            Probe::Unusable(uri, message) => {
                errors.push((uri, message));
                continue;
            }
            Probe::Already => {
                report.already += 1;
                continue;
            }
            Probe::Fresh(path, uri) => (path, uri),
        };
        let (meta, print) = match read {
            Some(Read::Stat(meta, print)) => (meta, print),
            Some(Read::StatFailed(message)) => {
                errors.push((uri, message));
                continue;
            }
            Some(Read::NotAFile) | None => continue,
        };
        let seen = Seen {
            uri: uri.clone(),
            size: meta.len() as i64,
            mtime: mtime_text(&meta),
        };
        let print = match print {
            Ok(print) => print,
            Err(err) => {
                // The path is recorded, with no fingerprint, so a later scan
                // can read it once it is readable.
                errors.push((uri, format!("cannot read: {err}")));
                entries.push(IngestEntry::New {
                    fingerprint: None,
                    blake3: None,
                    seen: vec![seen],
                });
                continue;
            }
        };
        plan_new_path(
            catalog,
            &mut entries,
            &mut hashed,
            &mut errors,
            report,
            &path,
            seen,
            print,
        )?;
    }
    let applied = catalog.apply_ingest(run, &entries, &errors)?;
    report.created += applied.created;
    report.located += applied.located;
    report.copies += applied.copies;
    report.errors.extend(errors);
    Ok(())
}

/// `each` over `items` on up to [`WORKERS`] threads, results in input order.
fn parallel_map<T: Sync, R: Send>(items: &[T], each: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let mut done: Vec<(usize, R)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..WORKERS.min(items.len()))
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(index) else {
                            return mine;
                        };
                        mine.push((index, each(item)));
                    }
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("an ingest worker panicked"))
            .collect()
    });
    done.sort_unstable_by_key(|(index, _)| *index);
    done.into_iter().map(|(_, result)| result).collect()
}

/// The command, what it covers, and the filters actually in effect. The
/// running count is separate: on a terminal it rewrites one line.
fn announce(command: &str, target: &str, filter: &PathFilter) {
    log_line(format!("{command} {target}"));
    if let Some(list) = join_patterns(filter.whitelist_patterns()) {
        log_line(format!("whitelist {list}"));
    }
    if let Some(list) = join_patterns(filter.blacklist_patterns()) {
        log_line(format!("blacklist {list}"));
    }
}

fn join_patterns(patterns: &[String]) -> Option<String> {
    if patterns.is_empty() {
        return None;
    }
    Some(
        patterns
            .iter()
            .map(|pattern| {
                if pattern.contains([',', ' ']) {
                    format!("\"{pattern}\"")
                } else {
                    pattern.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", "),
    )
}

struct Progress {
    files: u64,
    /// A count has been drawn, so a terminal line still needs its newline.
    shown: bool,
    tty: bool,
}

impl Progress {
    fn new() -> Self {
        Self {
            files: 0,
            shown: false,
            tty: std::io::stderr().is_terminal(),
        }
    }

    fn file(&mut self) {
        self.files += 1;
        if self.files.is_multiple_of(PROGRESS_EVERY) {
            let _ = paint_progress(&mut std::io::stderr(), self.files, self.tty);
            self.shown = true;
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        // The next stderr line (an error, or the shell prompt) must not
        // continue the rewritten count.
        if self.shown && self.tty {
            eprintln!();
        }
    }
}

/// `tty` rewrites the current line: carriage return, erase to the end, then
/// the count. A pipe cannot erase, so it gets a normal line.
fn paint_progress(out: &mut impl Write, files: u64, tty: bool) -> io::Result<()> {
    paint_line(out, &format!("{files} files seen"), tty)
}

fn paint_line(out: &mut impl Write, line: &str, tty: bool) -> io::Result<()> {
    if tty {
        write!(out, "\r\x1b[Kbpm: {line}")?;
    } else {
        writeln!(out, "bpm: {line}")?;
    }
    out.flush()
}

/// A scan's place in a list whose length is known before it starts.
struct ScanProgress {
    total: u64,
    total_bytes: u64,
    done: u64,
    bytes: u64,
    last: Instant,
    shown: bool,
    tty: bool,
}

impl ScanProgress {
    fn new() -> Self {
        Self {
            total: 0,
            total_bytes: 0,
            done: 0,
            bytes: 0,
            last: Instant::now(),
            shown: false,
            tty: std::io::stderr().is_terminal(),
        }
    }

    /// One location finished. Every [`PROGRESS_EVERY`] locations and the last
    /// one always draw.
    fn location(&mut self) {
        self.done += 1;
        if self.done.is_multiple_of(PROGRESS_EVERY) || self.done == self.total || self.due() {
            self.paint();
        }
    }

    /// Bytes of the current location read.
    fn read(&mut self, bytes: u64) {
        self.bytes += bytes;
        if self.due() {
            self.paint();
        }
    }

    fn due(&self) -> bool {
        let every = if self.tty {
            SCAN_REDRAW
        } else {
            SCAN_LOG_EVERY
        };
        self.last.elapsed() >= every
    }

    fn paint(&mut self) {
        let line = format!(
            "{}/{} locations, {} of {} read",
            self.done,
            self.total,
            human_bytes(self.bytes),
            human_bytes(self.total_bytes)
        );
        let _ = paint_line(&mut std::io::stderr(), &line, self.tty);
        self.last = Instant::now();
        self.shown = true;
    }
}

impl Drop for ScanProgress {
    fn drop(&mut self) {
        // As for Progress: end the rewritten line before anything else prints.
        if self.shown && self.tty {
            eprintln!();
        }
    }
}

/// A byte count in binary units, to one decimal place above a KiB.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn log_line(message: impl std::fmt::Display) {
    eprintln!("bpm: {message}");
    let _ = std::io::stderr().flush();
}

/// Decide what a new, fingerprinted path is: a copy of a file in this batch, a
/// new location of a file in the catalog, or a new file. Only a fingerprint
/// match leads to a full read, and only matching BLAKE3 values merge.
#[allow(clippy::too_many_arguments)]
fn plan_new_path(
    catalog: &Catalog,
    entries: &mut Vec<IngestEntry>,
    hashed: &mut HashMap<Uuid, (String, String)>,
    errors: &mut Vec<(String, String)>,
    report: &mut IngestReport,
    path: &Path,
    seen: Seen,
    print: Fingerprint,
) -> Result<(), Error> {
    let mut candidates = catalog.fingerprint_matches(seen.size, &print)?;
    // Rows written under an earlier scheme are compared in that scheme, so a
    // copy of a file ingested before the scheme changed is still found. This
    // reads the new path again only when a row of that size exists.
    for scheme in catalog.other_schemes(seen.size, &print.scheme)? {
        // A path that cannot be read here fails the full read below as well.
        if let Ok(Some(theirs)) = hash::fingerprint_in(&scheme, path, seen.size as u64) {
            candidates.extend(catalog.fingerprint_matches(seen.size, &theirs)?);
        }
    }
    let pending: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| match entry {
            IngestEntry::New {
                fingerprint: Some(other),
                seen: others,
                ..
            } if *other == print && others[0].size == seen.size => Some(index),
            _ => None,
        })
        .collect();
    if candidates.is_empty() && pending.is_empty() {
        entries.push(IngestEntry::New {
            fingerprint: Some(print),
            blake3: None,
            seen: vec![seen],
        });
        return Ok(());
    }
    let mine = match hash::digest(Path::new(&seen.uri), false, false) {
        Ok(digests) => digests.blake3,
        Err(err) => {
            // The fingerprint matched something, and this path could not be read
            // to confirm. Report the possible duplicate, and keep no fingerprint,
            // as for any path whose bytes could not be read.
            errors.push((seen.uri.clone(), format!("cannot read: {err}")));
            let mut of: Vec<String> = candidates
                .iter()
                .map(|candidate| candidate.file_id.to_string())
                .collect();
            for index in pending {
                if let IngestEntry::New { seen: others, .. } = &entries[index] {
                    of.push(others[0].uri.clone());
                }
            }
            report.possible_duplicates.push((seen.uri.clone(), of));
            entries.push(IngestEntry::New {
                fingerprint: None,
                blake3: None,
                seen: vec![seen],
            });
            return Ok(());
        }
    };
    let mut unhashable = Vec::new();
    for index in pending {
        let IngestEntry::New {
            blake3,
            seen: others,
            ..
        } = &mut entries[index]
        else {
            continue;
        };
        if blake3.is_none() {
            let first = &others[0];
            match hash::digest(Path::new(&first.uri), false, false) {
                Ok(digests) => *blake3 = Some(digests.blake3),
                Err(_) => {
                    unhashable.push(first.uri.clone());
                    continue;
                }
            }
        }
        if blake3.as_deref() == Some(mine.as_str()) {
            others.push(seen);
            return Ok(());
        }
    }
    for candidate in candidates {
        let theirs = match &candidate.blake3 {
            Some(stored) => Some((stored.clone(), None)),
            None => match hashed.get(&candidate.file_id) {
                Some((digest, uri)) => Some((digest.clone(), Some(uri.clone()))),
                None => hash_candidate(&candidate).map(|(digest, uri)| {
                    hashed.insert(candidate.file_id, (digest.clone(), uri.clone()));
                    (digest, Some(uri))
                }),
            },
        };
        let Some((theirs, verified)) = theirs else {
            unhashable.push(candidate.file_id.to_string());
            continue;
        };
        if theirs == mine {
            entries.push(IngestEntry::Located {
                file_id: candidate.file_id,
                blake3: mine,
                verified,
                fingerprint: print,
                seen,
            });
            return Ok(());
        }
    }
    if !unhashable.is_empty() {
        report
            .possible_duplicates
            .push((seen.uri.clone(), unhashable));
    }
    entries.push(IngestEntry::New {
        fingerprint: Some(print),
        blake3: Some(mine),
        seen: vec![seen],
    });
    Ok(())
}

/// BLAKE3 of an existing file with no stored digest, read from the first
/// location whose size and mtime still match what the catalog recorded. Bytes
/// that changed since then are not the file the catalog describes.
fn hash_candidate(candidate: &Candidate) -> Option<(String, String)> {
    candidate.locations.iter().find_map(|location| {
        let meta = fs::metadata(&location.uri).ok()?;
        if !meta.is_file()
            || meta.len() as i64 != location.size
            || mtime_text(&meta) != location.mtime
        {
            return None;
        }
        let digests = hash::digest(Path::new(&location.uri), false, false).ok()?;
        Some((digests.blake3, location.uri.clone()))
    })
}

pub fn scan(
    catalog: &mut Catalog,
    arg: &ScanArg,
    filter: &PathFilter,
    md5: bool,
) -> Result<ScanReport, Error> {
    let scope = match arg {
        ScanArg::All => ScanScope::default(),
        ScanArg::Backend(backend) => ScanScope {
            backend: Some(backend.clone()),
            root: None,
        },
        ScanArg::Path(path) => ScanScope {
            backend: Some(POSIX.to_string()),
            root: Some(utf8(&walk::location_path(path)?)?),
        },
    };
    announce(
        "scan",
        match arg {
            ScanArg::All => "all",
            ScanArg::Backend(backend) => backend,
            ScanArg::Path(_) => scope.root.as_deref().unwrap_or_default(),
        },
        filter,
    );
    let mut progress = scan_totals(catalog, &scope, filter)?;
    log_line(format!(
        "{} locations, {} to read",
        progress.total,
        human_bytes(progress.total_bytes)
    ));
    let run = catalog.start_run(
        RunKind::Scan,
        scope.backend.as_deref().unwrap_or(""),
        scope.root.as_deref().unwrap_or(""),
    )?;
    let mut report = ScanReport {
        run_id: Some(run.id),
        ..ScanReport::default()
    };
    let result = scan_pages(
        catalog,
        &run,
        &scope,
        filter,
        md5,
        &mut report,
        &mut progress,
    );
    drop(progress);
    let seen = report.seen;
    catalog.finish_run(run, result.is_ok(), seen, 0)?;
    result.map(|()| report)
}

fn scan_pages(
    catalog: &mut Catalog,
    run: &Run,
    scope: &ScanScope,
    filter: &PathFilter,
    md5: bool,
    report: &mut ScanReport,
    progress: &mut ScanProgress,
) -> Result<(), Error> {
    let mut after: Option<(String, String)> = None;
    loop {
        let page = catalog.scan_page(scope, after.as_ref(), BATCH)?;
        let Some(last) = page.last() else {
            return Ok(());
        };
        after = Some((last.backend.clone(), last.uri.clone()));
        let mut results = Vec::with_capacity(page.len());
        for target in page {
            if !in_filter(scope, filter, &target.uri) {
                continue;
            }
            report.seen += 1;
            // Only the local filesystem can be read in this milestone.
            if target.backend != POSIX {
                progress.location();
                continue;
            }
            let outcome = observe(&target, md5, progress);
            progress.location();
            results.push((target, outcome));
        }
        let states = catalog.apply_scan(run, &results)?;
        for ((target, outcome), drift) in results.iter().zip(states) {
            if let ScanOutcome::Error(message) = outcome {
                report.errors.push((target.uri.clone(), message.clone()));
                continue;
            }
            for state in drift {
                *report.counts.entry(state).or_default() += 1;
                if matches!(
                    state,
                    Drift::Missing | Drift::StatChanged | Drift::DigestMismatch
                ) {
                    report.drifted.push((state, target.uri.clone()));
                }
            }
        }
    }
}

/// Whether the path filters, matched against the scan root, keep a location.
fn in_filter(scope: &ScanScope, filter: &PathFilter, uri: &str) -> bool {
    match &scope.root {
        Some(root) => filter.allows(&relative(uri, root)),
        None => filter.allows(uri),
    }
}

/// The locations a scan will visit and the bytes it expects to read, from the
/// sizes the catalog last recorded. Only local locations are read.
fn scan_totals(
    catalog: &Catalog,
    scope: &ScanScope,
    filter: &PathFilter,
) -> Result<ScanProgress, Error> {
    let mut progress = ScanProgress::new();
    catalog.scan_sizes(scope, |backend, uri, size| {
        if !in_filter(scope, filter, uri) {
            return;
        }
        progress.total += 1;
        if backend == POSIX {
            progress.total_bytes += size.unwrap_or(0).max(0) as u64;
        }
    })?;
    Ok(progress)
}

/// Stat a location and, when it is there, read it once.
fn observe(target: &ScanTarget, md5: bool, progress: &mut ScanProgress) -> ScanOutcome {
    let meta = match fs::metadata(&target.uri) {
        Ok(meta) => meta,
        Err(err) if is_gone(&err) => return ScanOutcome::Missing,
        Err(err) => return ScanOutcome::Error(err.to_string()),
    };
    if !meta.is_file() {
        return ScanOutcome::Error("not a regular file".into());
    }
    let size = meta.len() as i64;
    let mtime = mtime_text(&meta);
    let stat_changed = Some(size) != target.last_size || mtime != target.last_mtime;
    // A row under an earlier scheme moves to the current one here, where the
    // bytes are read in full; the catalog keeps it only if BLAKE3 holds.
    let want_fingerprint =
        stat_changed || target.fingerprint_scheme.as_deref() != Some(hash::HEAD_SCHEME);
    let read = hash::digest_reporting(Path::new(&target.uri), md5, want_fingerprint, |bytes| {
        progress.read(bytes)
    });
    match read {
        Ok(digests) => ScanOutcome::Present {
            size,
            mtime,
            stat_changed,
            blake3: digests.blake3,
            md5: digests.md5,
            fingerprint: digests.fingerprint,
        },
        Err(err) => ScanOutcome::Error(format!("cannot read: {err}")),
    }
}

/// Accept the bytes now at one file's locations. Every present location is
/// read; they must hold the same bytes. This never covers more than the one
/// file named.
pub fn acknowledge(catalog: &mut Catalog, file: &FileRef, md5: bool) -> Result<AckReport, Error> {
    if let FileRef::Location { uri, .. } = file {
        if Path::new(uri).is_dir() {
            return Err(Error::AcknowledgeDirectory(PathBuf::from(uri)));
        }
    }
    let row = catalog.file(file)?;
    let has_md5 = row.digests.iter().any(|digest| digest.algorithm == "md5");
    let want_md5 = md5 || has_md5;
    let mut present = Vec::new();
    let mut missing = Vec::new();
    for location in row
        .locations
        .iter()
        .filter(|location| location.backend == POSIX)
    {
        let meta = match fs::metadata(&location.uri) {
            Ok(meta) => meta,
            Err(err) if is_gone(&err) => {
                missing.push(location.uri.clone());
                continue;
            }
            Err(err) => return Err(Error::Message(format!("{}: {err}", location.uri))),
        };
        if !meta.is_file() {
            return Err(Error::Message(format!(
                "{}: not a regular file",
                location.uri
            )));
        }
        let digests = hash::digest(Path::new(&location.uri), want_md5, true)
            .map_err(|err| Error::Message(format!("{}: cannot read: {err}", location.uri)))?;
        let seen = Seen {
            uri: location.uri.clone(),
            size: meta.len() as i64,
            mtime: mtime_text(&meta),
        };
        present.push((seen, digests));
    }
    let Some((_, first)) = present.first() else {
        return Err(Error::NoPresentLocation(row.id.to_string()));
    };
    if present
        .iter()
        .any(|(_, digests)| digests.blake3 != first.blake3)
    {
        let listing = present
            .iter()
            .map(|(seen, digests)| format!("  {}  blake3:{}", seen.uri, digests.blake3))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(Error::LocationsDisagree {
            id: row.id.to_string(),
            listing,
        });
    }
    let current = row
        .digests
        .iter()
        .find(|digest| digest.algorithm == "blake3")
        .map(|digest| digest.hex.as_str());
    let all_ok = row
        .locations
        .iter()
        .all(|location| location.drift() == [Drift::Ok]);
    if current == Some(first.blake3.as_str()) && all_ok && missing.is_empty() && (has_md5 || !md5) {
        return Ok(AckReport::NothingToDo(row));
    }
    let observed = AckObservation {
        file_id: row.id,
        blake3: first.blake3.clone(),
        md5: first.md5.clone(),
        fingerprint: first
            .fingerprint
            .clone()
            .expect("acknowledge asks for a fingerprint"),
        present: present.into_iter().map(|(seen, _)| seen).collect(),
        missing,
    };
    let outcome = catalog.acknowledge(&observed)?;
    let file = catalog.file(&FileRef::Id(row.id))?;
    Ok(AckReport::Done { file, outcome })
}

fn is_gone(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    )
}

/// A location's path relative to the scan root, for the path filters. The
/// root itself, when it is a file, is matched by its name.
fn relative(uri: &str, root: &str) -> String {
    let prefix = root.trim_end_matches('/');
    match uri
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('/'))
    {
        Some(rest) if !rest.is_empty() => rest.to_string(),
        _ => uri.rsplit('/').next().unwrap_or(uri).to_string(),
    }
}

/// UTC, ISO 8601, millisecond precision, `Z` suffix: the catalog's timestamp
/// form. Two stats compare equal when they agree to the millisecond.
fn mtime_text(meta: &fs::Metadata) -> Option<String> {
    meta.modified().ok().map(utc_text)
}

pub fn utc_text(time: SystemTime) -> String {
    DateTime::<Utc>::from(time)
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn utf8(path: &Path) -> Result<String, Error> {
    path.to_str()
        .map(String::from)
        .ok_or_else(|| Error::Message(format!("{} is not valid UTF-8", path.display())))
}

#[cfg(test)]
mod progress_tests {
    use super::{human_bytes, paint_progress};

    #[test]
    fn bytes_read_in_binary_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1536 << 20), "1.5 GiB");
        assert_eq!(human_bytes(100 << 40), "100.0 TiB");
    }

    #[test]
    fn a_terminal_rewrites_one_line() {
        let mut buf = Vec::new();
        paint_progress(&mut buf, 100, true).unwrap();
        paint_progress(&mut buf, 200, true).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert_eq!(
            text,
            "\r\x1b[Kbpm: 100 files seen\r\x1b[Kbpm: 200 files seen"
        );
        assert!(!text.contains('\n'));
    }

    #[test]
    fn a_pipe_keeps_a_line_per_update() {
        let mut buf = Vec::new();
        paint_progress(&mut buf, 100, false).unwrap();
        paint_progress(&mut buf, 200, false).unwrap();
        assert_eq!(
            String::from_utf8(buf).unwrap(),
            "bpm: 100 files seen\nbpm: 200 files seen\n"
        );
    }
}
