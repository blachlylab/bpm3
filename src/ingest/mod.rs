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
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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

#[derive(Debug, Default)]
pub struct IngestReport {
    pub run_id: Option<Uuid>,
    pub seen: u64,
    pub created: u64,
    pub located: u64,
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
    for item in Walker::new(root) {
        match item {
            WalkItem::Error { path, message } => {
                errors.push((path.to_string_lossy().into_owned(), message));
            }
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
                    )?;
                }
            }
        }
    }
    ingest_batch(catalog, run, paths, errors, report)
}

/// Plan one batch with no lock held, then apply it.
fn ingest_batch(
    catalog: &mut Catalog,
    run: &Run,
    paths: Vec<PathBuf>,
    mut errors: Vec<(String, String)>,
    report: &mut IngestReport,
) -> Result<(), Error> {
    let mut entries: Vec<IngestEntry> = Vec::new();
    let mut in_batch = HashSet::new();
    // BLAKE3 of existing files hashed during this batch, by file id.
    let mut hashed: HashMap<Uuid, (String, String)> = HashMap::new();
    for path in paths {
        let uri = match utf8(&path) {
            Ok(uri) => uri,
            Err(err) => {
                errors.push((path.to_string_lossy().into_owned(), err.to_string()));
                continue;
            }
        };
        if !in_batch.insert(uri.clone()) || catalog.location_exists(POSIX, &uri)? {
            continue;
        }
        let meta = match fs::metadata(&path) {
            Ok(meta) if meta.is_file() => meta,
            Ok(_) => continue,
            Err(err) => {
                errors.push((uri, err.to_string()));
                continue;
            }
        };
        let seen = Seen {
            uri: uri.clone(),
            size: meta.len() as i64,
            mtime: mtime_text(&meta),
        };
        let print = match hash::fingerprint(&path, meta.len()) {
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
            seen,
            print,
        )?;
    }
    let applied = catalog.apply_ingest(run, &entries, &errors)?;
    report.created += applied.created;
    report.located += applied.located;
    report.errors.extend(errors);
    Ok(())
}

/// Decide what a new, fingerprinted path is: a copy of a file in this batch, a
/// new location of a file in the catalog, or a new file. Only a fingerprint
/// match leads to a full read, and only matching BLAKE3 values merge.
fn plan_new_path(
    catalog: &Catalog,
    entries: &mut Vec<IngestEntry>,
    hashed: &mut HashMap<Uuid, (String, String)>,
    errors: &mut Vec<(String, String)>,
    report: &mut IngestReport,
    seen: Seen,
    print: Fingerprint,
) -> Result<(), Error> {
    let candidates = catalog.fingerprint_matches(seen.size, &print)?;
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
    let mine = match hash::digest(Path::new(&seen.uri), seen.size as u64, false, false) {
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
            match hash::digest(Path::new(&first.uri), first.size as u64, false, false) {
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
        let digests = hash::digest(Path::new(&location.uri), meta.len(), false, false).ok()?;
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
    let run = catalog.start_run(
        RunKind::Scan,
        scope.backend.as_deref().unwrap_or(""),
        scope.root.as_deref().unwrap_or(""),
    )?;
    let mut report = ScanReport {
        run_id: Some(run.id),
        ..ScanReport::default()
    };
    let result = scan_pages(catalog, &run, &scope, filter, md5, &mut report);
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
            let rel = match &scope.root {
                Some(root) => relative(&target.uri, root),
                None => target.uri.clone(),
            };
            if !filter.allows(&rel) {
                continue;
            }
            report.seen += 1;
            // Only the local filesystem can be read in this milestone.
            if target.backend != POSIX {
                continue;
            }
            let outcome = observe(&target, md5);
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

/// Stat a location and, when it is there, read it once.
fn observe(target: &ScanTarget, md5: bool) -> ScanOutcome {
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
    let want_fingerprint = stat_changed || !target.has_fingerprint;
    match hash::digest(Path::new(&target.uri), meta.len(), md5, want_fingerprint) {
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
        let digests = hash::digest(Path::new(&location.uri), meta.len(), want_md5, true)
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
