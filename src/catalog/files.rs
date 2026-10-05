//! File rows: ingest batches, scan observations, acknowledge, links, location
//! and file deletes, and file queries.
//!
//! Hashing happens in [`crate::ingest`] with no lock held. The methods here
//! that take the write lock only apply what was already computed, so the lock
//! is held for the length of one batch's inserts.

use std::collections::{HashMap, HashSet};

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};
use uuid::Uuid;

use super::runs::Run;
use super::{
    Catalog, begin, collect_descendants_seen, load_index, resolve_all, resolve_one, timestamp,
};
use crate::error::Error;
use crate::model::{
    DigestRow, Drift, FileRef, FileRow, Fingerprint, LinkRow, LocationRow, NodeType, POSIX, Paged,
    Summary, Window,
};

/// One path as a command saw it: where, and what stat said.
#[derive(Debug, Clone)]
pub struct Seen {
    pub uri: String,
    pub size: i64,
    pub mtime: Option<String>,
}

/// A file already in the catalog with the same size and fingerprint as a new
/// path. `locations` are its present posix locations whose stat has not
/// changed, with what was last recorded for them.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub file_id: Uuid,
    pub blake3: Option<String>,
    pub locations: Vec<Seen>,
}

/// What ingest decided for one new path, applied in one batch.
#[derive(Debug, Clone)]
pub enum IngestEntry {
    /// A new file id. More than one path means the batch found copies with the
    /// same BLAKE3. `blake3` is set when the bytes were hashed for that test.
    New {
        fingerprint: Option<Fingerprint>,
        blake3: Option<String>,
        seen: Vec<Seen>,
    },
    /// A new location of an existing file whose BLAKE3 matched. When the file
    /// had no BLAKE3, `verified` is the location whose bytes were hashed to
    /// compare, and that digest is stored as the file's first.
    Located {
        file_id: Uuid,
        blake3: String,
        verified: Option<String>,
        fingerprint: Fingerprint,
        seen: Seen,
    },
}

#[derive(Debug, Default, Clone, Copy)]
pub struct IngestApplied {
    pub created: u64,
    pub located: u64,
}

/// Which locations a scan reads. `root` is an absolute path; a location is in
/// scope when it is that path or under it.
#[derive(Debug, Default, Clone)]
pub struct ScanScope {
    pub backend: Option<String>,
    pub root: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ScanTarget {
    pub file_id: Uuid,
    pub backend: String,
    pub uri: String,
    pub last_size: Option<i64>,
    pub last_mtime: Option<String>,
    pub has_fingerprint: bool,
}

#[derive(Debug, Clone)]
pub enum ScanOutcome {
    Missing,
    Error(String),
    Present {
        size: i64,
        mtime: Option<String>,
        stat_changed: bool,
        blake3: String,
        md5: Option<String>,
        /// Set when the bytes were read for a new size or mtime, or when the
        /// file had no fingerprint.
        fingerprint: Option<Fingerprint>,
    },
}

/// What acknowledge read at the file's locations. Every present location held
/// these bytes.
#[derive(Debug, Clone)]
pub struct AckObservation {
    pub file_id: Uuid,
    pub blake3: String,
    pub md5: Option<String>,
    pub fingerprint: Fingerprint,
    pub present: Vec<Seen>,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckOutcome {
    /// The bytes differ from the current digest. This generation is now current.
    NewGeneration(i64),
    /// The bytes match the current digest. Only the stat was accepted.
    SameBytes(i64),
}

#[derive(Debug, Default, Clone)]
pub struct FileQuery {
    pub under: Option<String>,
    pub role: Option<String>,
    pub unlinked: bool,
    pub drift: Vec<Drift>,
    /// `(algorithm, lowercase hex)`. Matches a current digest.
    pub digest: Option<(String, String)>,
}

impl Catalog {
    pub fn location_exists(&self, backend: &str, uri: &str) -> Result<bool, Error> {
        location_exists(&self.conn, backend, uri)
    }

    /// Files with this size and the same fingerprint scheme and value.
    pub fn fingerprint_matches(
        &self,
        size: i64,
        print: &Fingerprint,
    ) -> Result<Vec<Candidate>, Error> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id FROM files
             WHERE size_bytes = ? AND fingerprint_scheme = ? AND fingerprint = ?
             ORDER BY id",
        )?;
        let ids = stmt
            .query_map(params![size, print.scheme, print.hex], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut candidates = Vec::with_capacity(ids.len());
        for id in ids {
            let file_id = parse_id(&id)?;
            let mut stmt = self.conn.prepare_cached(
                "SELECT uri, last_size, last_mtime FROM file_locations
                 WHERE file_id = ? AND backend = 'posix' AND presence = 'present'
                   AND stat_state <> 'changed'
                 ORDER BY uri",
            )?;
            let locations = stmt
                .query_map([&id], |row| {
                    Ok(Seen {
                        uri: row.get(0)?,
                        size: row.get::<_, Option<i64>>(1)?.unwrap_or(-1),
                        mtime: row.get(2)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            candidates.push(Candidate {
                file_id,
                blake3: current_digest(&self.conn, &id, "blake3")?,
                locations,
            });
        }
        Ok(candidates)
    }

    /// Apply one ingest batch under the write lock. A path another process
    /// recorded since the batch was planned is skipped. A merge whose file
    /// gained a different BLAKE3 meanwhile becomes a new file instead.
    pub fn apply_ingest(
        &mut self,
        run: &Run,
        entries: &[IngestEntry],
        errors: &[(String, String)],
    ) -> Result<IngestApplied, Error> {
        self.require_write()?;
        let stamp = timestamp();
        let tx = begin(&mut self.conn, &self.path)?;
        let mut applied = IngestApplied::default();
        for entry in entries {
            match entry {
                IngestEntry::New {
                    fingerprint,
                    blake3,
                    seen,
                } => {
                    let mut fresh = Vec::with_capacity(seen.len());
                    for one in seen {
                        if !location_exists(&tx, POSIX, &one.uri)? {
                            fresh.push(one);
                        }
                    }
                    if fresh.is_empty() {
                        continue;
                    }
                    // Another ingest may have committed these bytes while this
                    // batch was hashed. Only a BLAKE3 match merges.
                    if let Some(blake3) = blake3 {
                        if let Some(existing) = file_with_digest(&tx, "blake3", blake3)? {
                            for one in &fresh {
                                insert_location(&tx, &existing, one, "match", &stamp)?;
                            }
                            applied.located += fresh.len() as u64;
                            continue;
                        }
                    }
                    insert_file(&tx, fingerprint.as_ref(), blake3.as_deref(), &fresh, &stamp)?;
                    applied.created += 1;
                    applied.located += fresh.len() as u64;
                }
                IngestEntry::Located {
                    file_id,
                    blake3,
                    verified,
                    fingerprint,
                    seen,
                } => {
                    if location_exists(&tx, POSIX, &seen.uri)? {
                        continue;
                    }
                    let id = file_id.to_string();
                    let exists: bool = tx.query_row(
                        "SELECT EXISTS (SELECT 1 FROM files WHERE id = ?)",
                        [&id],
                        |row| row.get(0),
                    )?;
                    let current = if exists {
                        current_digest(&tx, &id, "blake3")?
                    } else {
                        None
                    };
                    match current {
                        Some(current) if current == *blake3 => {}
                        None if exists => {
                            let generation = current_generation(&tx, &id)?;
                            insert_digest(&tx, &id, "blake3", blake3, generation, &stamp)?;
                            if let Some(uri) = verified {
                                tx.execute(
                                    "UPDATE file_locations SET digest_state = 'match'
                                     WHERE backend = 'posix' AND uri = ? AND file_id = ?
                                       AND digest_state = 'unverified'",
                                    params![uri, id],
                                )?;
                            }
                        }
                        _ => {
                            insert_file(&tx, Some(fingerprint), Some(blake3), &[seen], &stamp)?;
                            applied.created += 1;
                            applied.located += 1;
                            continue;
                        }
                    }
                    insert_location(&tx, &id, seen, "match", &stamp)?;
                    applied.located += 1;
                }
            }
        }
        insert_errors(&tx, run, errors)?;
        tx.commit()?;
        Ok(applied)
    }

    /// Up to `limit` locations in scope after `after`, in `(backend, uri)`
    /// order. Each page is its own short read, so no read transaction stays
    /// open across a run.
    pub fn scan_page(
        &self,
        scope: &ScanScope,
        after: Option<&(String, String)>,
        limit: usize,
    ) -> Result<Vec<ScanTarget>, Error> {
        let mut sql = String::from(
            "SELECT l.file_id, l.backend, l.uri, l.last_size, l.last_mtime, f.fingerprint IS NOT NULL
             FROM file_locations l JOIN files f ON f.id = l.file_id WHERE 1 = 1",
        );
        let mut values: Vec<Value> = Vec::new();
        if let Some(backend) = &scope.backend {
            sql.push_str(" AND l.backend = ?");
            values.push(Value::Text(backend.clone()));
        }
        if let Some(root) = &scope.root {
            // The location itself, or anything below it: a range on the key.
            let prefix = if root.ends_with('/') {
                root.clone()
            } else {
                format!("{root}/")
            };
            let mut upper = prefix.clone();
            upper.pop();
            upper.push('0');
            sql.push_str(" AND (l.uri = ? OR (l.uri >= ? AND l.uri < ?))");
            values.push(Value::Text(root.clone()));
            values.push(Value::Text(prefix));
            values.push(Value::Text(upper));
        }
        if let Some((backend, uri)) = after {
            sql.push_str(" AND (l.backend, l.uri) > (?, ?)");
            values.push(Value::Text(backend.clone()));
            values.push(Value::Text(uri.clone()));
        }
        sql.push_str(" ORDER BY l.backend, l.uri LIMIT ?");
        values.push(Value::Integer(limit as i64));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt
            .query_map(params_from_iter(values), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    ScanTarget {
                        file_id: Uuid::nil(),
                        backend: row.get(1)?,
                        uri: row.get(2)?,
                        last_size: row.get(3)?,
                        last_mtime: row.get(4)?,
                        has_fingerprint: row.get(5)?,
                    },
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, mut target)| {
                target.file_id = parse_id(&id)?;
                Ok(target)
            })
            .collect()
    }

    /// Apply one scan batch under the write lock and return each location's
    /// drift states afterwards (empty for an error or a location removed since
    /// the page was read).
    ///
    /// `stat_changed` stays set until acknowledge, so a second scan does not
    /// clear the evidence of the first. The first read of a file records its
    /// BLAKE3, whatever the stat says; after that a different read is
    /// `digest_mismatch` and the stored digest stays. Within a batch, locations
    /// whose stat did not change are applied first, so an unchanged copy, when
    /// there is one, supplies that first digest. MD5 is recorded only beside a
    /// BLAKE3 that holds.
    ///
    /// The file row's size, mtime, and fingerprint describe the bytes of the
    /// current digest, because ingest matches duplicates on them. A location
    /// whose bytes do not hold that digest keeps its new stat on the location
    /// only; acknowledge is what moves it onto the file.
    pub fn apply_scan(
        &mut self,
        run: &Run,
        results: &[(ScanTarget, ScanOutcome)],
    ) -> Result<Vec<Vec<Drift>>, Error> {
        self.require_write()?;
        let stamp = timestamp();
        let tx = begin(&mut self.conn, &self.path)?;
        let mut states = vec![Vec::new(); results.len()];
        let mut errors = Vec::new();
        let mut order: Vec<usize> = (0..results.len()).collect();
        order.sort_by_key(|&index| {
            matches!(
                results[index].1,
                ScanOutcome::Present {
                    stat_changed: true,
                    ..
                }
            )
        });
        for index in order {
            let (target, outcome) = &results[index];
            let id = target.file_id.to_string();
            let Some(stat_state) = tx
                .query_row(
                    "SELECT stat_state FROM file_locations WHERE backend = ? AND uri = ? AND file_id = ?",
                    params![target.backend, target.uri, id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            else {
                continue;
            };
            match outcome {
                ScanOutcome::Error(message) => {
                    errors.push((target.uri.clone(), message.clone()));
                    continue;
                }
                ScanOutcome::Missing => {
                    tx.execute(
                        "UPDATE file_locations SET presence = 'missing' WHERE backend = ? AND uri = ?",
                        params![target.backend, target.uri],
                    )?;
                }
                ScanOutcome::Present {
                    size,
                    mtime,
                    stat_changed,
                    blake3,
                    md5,
                    fingerprint,
                } => {
                    let stat_state = if *stat_changed || stat_state == "changed" {
                        "changed"
                    } else {
                        "unchanged"
                    };
                    let by_blake3 = compare_digest(&tx, &id, "blake3", blake3, true, &stamp)?;
                    let blake3_holds = matches!(by_blake3, Compared::Match | Compared::Recorded);
                    if blake3_holds {
                        if *stat_changed {
                            tx.execute(
                                "UPDATE files SET size_bytes = ?, mtime = ? WHERE id = ?",
                                params![size, mtime, id],
                            )?;
                        }
                        if let Some(print) = fingerprint {
                            tx.execute(
                                "UPDATE files SET fingerprint = ?, fingerprint_scheme = ? WHERE id = ?",
                                params![print.hex, print.scheme, id],
                            )?;
                        }
                    }
                    let by_md5 = match md5 {
                        Some(md5) => compare_digest(&tx, &id, "md5", md5, blake3_holds, &stamp)?,
                        None => Compared::Skipped,
                    };
                    let digest_state =
                        if by_blake3 == Compared::Mismatch || by_md5 == Compared::Mismatch {
                            "mismatch"
                        } else if blake3_holds {
                            "match"
                        } else {
                            "unverified"
                        };
                    tx.execute(
                        "UPDATE file_locations SET presence = 'present', last_seen_at = ?,
                           last_size = ?, last_mtime = ?, stat_state = ?, digest_state = ?
                         WHERE backend = ? AND uri = ?",
                        params![
                            stamp,
                            size,
                            mtime,
                            stat_state,
                            digest_state,
                            target.backend,
                            target.uri
                        ],
                    )?;
                }
            }
            let drift = tx.query_row(
                "SELECT presence, stat_state, digest_state FROM file_locations WHERE backend = ? AND uri = ?",
                params![target.backend, target.uri],
                |row| {
                    Ok(Drift::of(
                        &row.get::<_, String>(0)?,
                        &row.get::<_, String>(1)?,
                        &row.get::<_, String>(2)?,
                    ))
                },
            )?;
            states[index] = drift;
        }
        insert_errors(&tx, run, &errors)?;
        tx.commit()?;
        Ok(states)
    }

    /// One file, by id or by a location, with its current digests, locations,
    /// and links.
    pub fn file(&mut self, file: &FileRef) -> Result<FileRow, Error> {
        let snapshot = self.conn.transaction()?;
        let id = resolve_file(&snapshot, file)?;
        load_file(&snapshot, &id)
    }

    /// Every digest row of a file, oldest generation first.
    pub fn digest_history(&mut self, file: &FileRef) -> Result<Vec<DigestRow>, Error> {
        let snapshot = self.conn.transaction()?;
        let id = resolve_file(&snapshot, file)?;
        let mut stmt = snapshot.prepare(
            "SELECT algorithm, digest, source, generation, current FROM file_digests
             WHERE file_id = ? ORDER BY generation, algorithm",
        )?;
        let rows = stmt
            .query_map([&id], digest_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Accept the bytes acknowledge read as the file's current state. New bytes
    /// start the next generation: every current digest of the file is retired
    /// and the observed ones become current. Bytes equal to the current BLAKE3
    /// keep the generation and only accept the stat.
    pub fn acknowledge(&mut self, observed: &AckObservation) -> Result<AckOutcome, Error> {
        self.require_write()?;
        let stamp = timestamp();
        let id = observed.file_id.to_string();
        let Some(first) = observed.present.first() else {
            return Err(Error::NoPresentLocation(id));
        };
        let tx = begin(&mut self.conn, &self.path)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM files WHERE id = ?)",
            [&id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(Error::FileNotFound(id));
        }
        let same = current_digest(&tx, &id, "blake3")?.as_deref() == Some(observed.blake3.as_str());
        let outcome = if same {
            let generation = current_generation(&tx, &id)?;
            if let Some(md5) = &observed.md5 {
                compare_digest(&tx, &id, "md5", md5, true, &stamp)?;
            }
            AckOutcome::SameBytes(generation)
        } else {
            let generation: i64 = tx.query_row(
                "SELECT COALESCE(MAX(generation), 0) + 1 FROM file_digests WHERE file_id = ?",
                [&id],
                |row| row.get(0),
            )?;
            tx.execute(
                "UPDATE file_digests SET current = 0 WHERE file_id = ? AND current = 1",
                [&id],
            )?;
            insert_digest(&tx, &id, "blake3", &observed.blake3, generation, &stamp)?;
            if let Some(md5) = &observed.md5 {
                insert_digest(&tx, &id, "md5", md5, generation, &stamp)?;
            }
            AckOutcome::NewGeneration(generation)
        };
        tx.execute(
            "UPDATE files SET size_bytes = ?, mtime = ?, fingerprint = ?, fingerprint_scheme = ?
             WHERE id = ?",
            params![
                first.size,
                first.mtime,
                observed.fingerprint.hex,
                observed.fingerprint.scheme,
                id
            ],
        )?;
        for seen in &observed.present {
            tx.execute(
                "UPDATE file_locations SET presence = 'present', last_seen_at = ?, last_size = ?,
                   last_mtime = ?, stat_state = 'unchanged', digest_state = 'match'
                 WHERE backend = 'posix' AND uri = ? AND file_id = ?",
                params![stamp, seen.size, seen.mtime, seen.uri, id],
            )?;
        }
        for uri in &observed.missing {
            // A copy that is gone was not compared with the accepted bytes.
            let digest_state = if same { None } else { Some("unverified") };
            tx.execute(
                "UPDATE file_locations SET presence = 'missing',
                   digest_state = COALESCE(?, digest_state)
                 WHERE backend = 'posix' AND uri = ? AND file_id = ?",
                params![digest_state, uri, id],
            )?;
        }
        tx.commit()?;
        Ok(outcome)
    }

    /// Attach a file to an entity. Linking the same pair again replaces its role.
    pub fn link(&mut self, file: &FileRef, entity: &str, role: &str) -> Result<(), Error> {
        self.require_write()?;
        if role.trim().is_empty() {
            return Err(Error::Message("a link role cannot be empty".into()));
        }
        let tx = begin(&mut self.conn, &self.path)?;
        let index = load_index(&tx)?;
        let node = resolve_one(&tx, &index, entity)?;
        let (node_type, node_id) = (node.node_type, node.id);
        let file_id = resolve_file(&tx, file)?;
        tx.execute(
            "INSERT INTO file_links (file_id, node_type, node_id, role) VALUES (?, ?, ?, ?)
             ON CONFLICT (file_id, node_type, node_id) DO UPDATE SET role = excluded.role",
            params![file_id, node_type.slug(), node_id.to_string(), role],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn unlink(&mut self, file: &FileRef, entity: &str) -> Result<(), Error> {
        self.require_write()?;
        let tx = begin(&mut self.conn, &self.path)?;
        let index = load_index(&tx)?;
        let node = resolve_one(&tx, &index, entity)?;
        let (node_type, node_id) = (node.node_type, node.id);
        let file_id = resolve_file(&tx, file)?;
        let removed = tx.execute(
            "DELETE FROM file_links WHERE file_id = ? AND node_type = ? AND node_id = ?",
            params![file_id, node_type.slug(), node_id.to_string()],
        )?;
        if removed == 0 {
            return Err(Error::NotLinked(entity.to_string()));
        }
        tx.commit()?;
        Ok(())
    }

    /// Remove one location. The file and its other locations stay.
    pub fn delete_location(&mut self, backend: &str, uri: &str) -> Result<(), Error> {
        self.require_write()?;
        let tx = begin(&mut self.conn, &self.path)?;
        let removed = tx.execute(
            "DELETE FROM file_locations WHERE backend = ? AND uri = ?",
            params![backend, uri],
        )?;
        if removed == 0 {
            return Err(Error::LocationNotFound(uri.to_string()));
        }
        tx.commit()?;
        Ok(())
    }

    /// Remove a file row with its locations, digests, and metadata. Refused
    /// while links remain unless `cascade` removes them too. Bytes on disk are
    /// never touched.
    pub fn delete_file(&mut self, file: &FileRef, cascade: bool) -> Result<(), Error> {
        self.require_write()?;
        let tx = begin(&mut self.conn, &self.path)?;
        let id = resolve_file(&tx, file)?;
        let links: i64 = tx.query_row(
            "SELECT COUNT(*) FROM file_links WHERE file_id = ?",
            [&id],
            |row| row.get(0),
        )?;
        if links > 0 && !cascade {
            return Err(Error::FileLinked);
        }
        for table in [
            "file_links",
            "file_metadata",
            "file_digests",
            "file_locations",
        ] {
            tx.execute(&format!("DELETE FROM {table} WHERE file_id = ?"), [&id])?;
        }
        tx.execute("DELETE FROM files WHERE id = ?", [&id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn query_files(&mut self, query: &FileQuery) -> Result<Vec<FileRow>, Error> {
        Ok(self.query_files_page(query, Window::ALL)?.rows)
    }

    /// One window of a file query, in file-id order. Details are read only for
    /// the files in the window.
    pub fn query_files_page(
        &mut self,
        query: &FileQuery,
        window: Window,
    ) -> Result<Paged<FileRow>, Error> {
        // One read transaction, so the ids and their details are one snapshot.
        let snapshot = self.conn.transaction()?;
        let mut clauses = Vec::new();
        let mut values: Vec<Value> = Vec::new();
        if let Some(under) = &query.under {
            let index = load_index(&snapshot)?;
            let mut nodes = Vec::new();
            let mut seen = HashSet::new();
            for root in resolve_all(&snapshot, &index, under)? {
                if seen.insert(root) {
                    nodes.push(root);
                }
                collect_descendants_seen(root, &index, &mut nodes, &mut seen);
            }
            snapshot.execute("DROP TABLE IF EXISTS temp.bpm_under", [])?;
            snapshot.execute(
                "CREATE TEMP TABLE bpm_under (node_type TEXT, id TEXT, PRIMARY KEY (node_type, id))",
                [],
            )?;
            {
                let mut insert =
                    snapshot.prepare("INSERT INTO temp.bpm_under (node_type, id) VALUES (?, ?)")?;
                for id in nodes {
                    insert.execute(params![index.get(id).node_type.slug(), id.to_string()])?;
                }
            }
            // The entity set drives the join (`CROSS JOIN`), through the
            // `(node_type, node_id)` index on links.
            let mut clause = String::from(
                "f.id IN (SELECT l.file_id FROM temp.bpm_under u CROSS JOIN file_links l
                 WHERE l.node_type = u.node_type AND l.node_id = u.id",
            );
            if let Some(role) = &query.role {
                clause.push_str(" AND l.role = ?");
                values.push(Value::Text(role.clone()));
            }
            clause.push(')');
            clauses.push(clause);
        } else if let Some(role) = &query.role {
            clauses.push("f.id IN (SELECT file_id FROM file_links WHERE role = ?)".into());
            values.push(Value::Text(role.clone()));
        }
        if query.unlinked {
            clauses.push("NOT EXISTS (SELECT 1 FROM file_links l WHERE l.file_id = f.id)".into());
        }
        if let Some((algorithm, hex)) = &query.digest {
            clauses.push(
                "f.id IN (SELECT file_id FROM file_digests WHERE algorithm = ? AND digest = ? AND current = 1)"
                    .into(),
            );
            values.push(Value::Text(algorithm.clone()));
            values.push(Value::Text(hex.clone()));
        }
        if !query.drift.is_empty() {
            let states: Vec<&str> = query
                .drift
                .iter()
                .map(|state| drift_predicate(*state))
                .collect();
            clauses.push(format!(
                "EXISTS (SELECT 1 FROM file_locations x WHERE x.file_id = f.id AND ({}))",
                states.join(" OR ")
            ));
        }
        let mut sql = String::from("SELECT f.id FROM files f");
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY f.id");
        let ids = {
            let mut stmt = snapshot.prepare(&sql)?;
            stmt.query_map(params_from_iter(values), |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
        };
        let total = ids.len();
        let rows = load_files(&snapshot, &window.apply(ids))?;
        if query.under.is_some() {
            snapshot.execute("DROP TABLE IF EXISTS temp.bpm_under", [])?;
        }
        Ok(Paged { total, rows })
    }

    /// Counts for the home page and `bpm query summary`. Each figure is one
    /// grouped read, so the cost is a few table scans.
    pub fn summary(&mut self) -> Result<Summary, Error> {
        let snapshot = self.conn.transaction()?;
        let mut summary = Summary::default();
        for node_type in NodeType::ALL {
            let count: i64 = snapshot.query_row(
                "SELECT COUNT(*) FROM entities WHERE node_type = ?",
                [node_type.slug()],
                |row| row.get(0),
            )?;
            summary.entities.push((node_type, count));
        }
        (summary.files, summary.bytes) = snapshot.query_row(
            "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0) FROM files",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        summary.unlinked = snapshot.query_row(
            "SELECT COUNT(*) FROM files f
             WHERE NOT EXISTS (SELECT 1 FROM file_links l WHERE l.file_id = f.id)",
            [],
            |row| row.get(0),
        )?;
        {
            // Grouping links by (file_id, node_type) follows the links key, so
            // it needs no sort; a DISTINCT over the join took twice as long.
            let mut stmt = snapshot.prepare(
                "SELECT t.node_type, COUNT(*), COALESCE(SUM(f.size_bytes), 0)
                 FROM (SELECT file_id, node_type FROM file_links GROUP BY file_id, node_type) t
                 JOIN files f ON f.id = t.file_id
                 GROUP BY t.node_type",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                if let Some(node_type) = NodeType::parse(&row.get::<_, String>(0)?) {
                    summary
                        .linked_by_type
                        .push((node_type, row.get(1)?, row.get(2)?));
                }
            }
            summary
                .linked_by_type
                .sort_by_key(|(node_type, _, _)| node_type.rank());
        }
        {
            // One pass over the locations for both the backends and the drift
            // states, which come from the columns.
            let mut counts = std::collections::BTreeMap::new();
            let mut backends = std::collections::BTreeMap::new();
            let mut stmt = snapshot.prepare(
                "SELECT backend, presence, stat_state, digest_state, COUNT(*) FROM file_locations
                 GROUP BY backend, presence, stat_state, digest_state",
            )?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let n: i64 = row.get(4)?;
                *backends.entry(row.get::<_, String>(0)?).or_insert(0) += n;
                for state in Drift::of(
                    &row.get::<_, String>(1)?,
                    &row.get::<_, String>(2)?,
                    &row.get::<_, String>(3)?,
                ) {
                    *counts.entry(state).or_insert(0) += n;
                }
            }
            summary.locations_by_backend = backends.into_iter().collect();
            summary.drift = Drift::ALL
                .into_iter()
                .map(|state| (state, counts.get(&state).copied().unwrap_or(0)))
                .collect();
        }
        summary.sample_kind = grouped(
            &snapshot,
            "SELECT value, COUNT(*) FROM entity_metadata
             WHERE key = 'sample_kind' AND node_type = 'sample' GROUP BY value ORDER BY value",
        )?;
        summary.assay = grouped(
            &snapshot,
            "SELECT value, COUNT(*) FROM entity_metadata
             WHERE key = 'assay' GROUP BY value ORDER BY value",
        )?;
        Ok(summary)
    }
}

fn grouped(conn: &Connection, sql: &str) -> Result<Vec<(String, i64)>, Error> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// One window of the files linked to one entity itself, in file-id order.
pub(super) fn linked_files(
    conn: &Connection,
    node_type: NodeType,
    node_id: Uuid,
    window: Window,
) -> Result<Paged<FileRow>, Error> {
    let id = node_id.to_string();
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM file_links WHERE node_type = ? AND node_id = ?",
        params![node_type.slug(), id],
        |row| row.get(0),
    )?;
    let ids = {
        let mut stmt = conn.prepare(
            "SELECT file_id FROM file_links WHERE node_type = ? AND node_id = ?
             ORDER BY file_id LIMIT ? OFFSET ?",
        )?;
        stmt.query_map(
            params![
                node_type.slug(),
                id,
                i64::try_from(window.limit).unwrap_or(i64::MAX),
                i64::try_from(window.offset).unwrap_or(i64::MAX)
            ],
            |row| row.get::<_, String>(0),
        )?
        .collect::<Result<Vec<_>, _>>()?
    };
    Ok(Paged {
        total: total as usize,
        rows: load_files(conn, &ids)?,
    })
}

/// Columns of `file_locations x` for one operator state, as in the
/// architecture overview §6.
fn drift_predicate(state: Drift) -> &'static str {
    match state {
        Drift::Missing => "x.presence = 'missing'",
        Drift::StatChanged => "(x.presence = 'present' AND x.stat_state = 'changed')",
        Drift::DigestMismatch => "x.digest_state = 'mismatch'",
        Drift::Ok => {
            "(x.presence = 'present' AND x.stat_state = 'unchanged' AND x.digest_state = 'match')"
        }
        Drift::Unverified => {
            "(x.presence = 'present' AND x.stat_state <> 'changed' AND x.digest_state <> 'mismatch'
              AND NOT (x.stat_state = 'unchanged' AND x.digest_state = 'match'))"
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compared {
    Match,
    Mismatch,
    Recorded,
    Skipped,
}

/// Compare a computed digest with the current one of that algorithm. With no
/// current digest it is recorded in the file's current generation when
/// `may_record`, and skipped otherwise.
fn compare_digest(
    conn: &Connection,
    file_id: &str,
    algorithm: &str,
    hex: &str,
    may_record: bool,
    stamp: &str,
) -> Result<Compared, Error> {
    Ok(match current_digest(conn, file_id, algorithm)? {
        Some(current) if current == hex => Compared::Match,
        Some(_) => Compared::Mismatch,
        None if may_record => {
            let generation = current_generation(conn, file_id)?;
            insert_digest(conn, file_id, algorithm, hex, generation, stamp)?;
            Compared::Recorded
        }
        None => Compared::Skipped,
    })
}

fn file_with_digest(
    conn: &Connection,
    algorithm: &str,
    hex: &str,
) -> Result<Option<String>, Error> {
    let mut stmt = conn.prepare_cached(
        "SELECT file_id FROM file_digests
         WHERE algorithm = ? AND digest = ? AND current = 1
         ORDER BY file_id LIMIT 1",
    )?;
    Ok(stmt
        .query_row(params![algorithm, hex], |row| row.get(0))
        .optional()?)
}

fn location_exists(conn: &Connection, backend: &str, uri: &str) -> Result<bool, Error> {
    let mut stmt = conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM file_locations WHERE backend = ? AND uri = ?)",
    )?;
    Ok(stmt.query_row(params![backend, uri], |row| row.get(0))?)
}

fn current_digest(
    conn: &Connection,
    file_id: &str,
    algorithm: &str,
) -> Result<Option<String>, Error> {
    let mut stmt = conn.prepare_cached(
        "SELECT digest FROM file_digests WHERE file_id = ? AND algorithm = ? AND current = 1",
    )?;
    Ok(stmt
        .query_row(params![file_id, algorithm], |row| row.get(0))
        .optional()?)
}

/// The generation the file's current digests belong to. With no current
/// digest, the one after the newest retired generation, or 1.
fn current_generation(conn: &Connection, file_id: &str) -> Result<i64, Error> {
    let mut stmt = conn.prepare_cached(
        "SELECT COALESCE(
           (SELECT MAX(generation) FROM file_digests WHERE file_id = ?1 AND current = 1),
           (SELECT MAX(generation) + 1 FROM file_digests WHERE file_id = ?1),
           1)",
    )?;
    Ok(stmt.query_row([file_id], |row| row.get(0))?)
}

fn insert_digest(
    conn: &Connection,
    file_id: &str,
    algorithm: &str,
    hex: &str,
    generation: i64,
    stamp: &str,
) -> Result<(), Error> {
    let mut stmt = conn.prepare_cached(
        "INSERT INTO file_digests (file_id, algorithm, digest, source, generation, current, observed_at)
         VALUES (?, ?, ?, 'computed', ?, 1, ?)",
    )?;
    stmt.execute(params![file_id, algorithm, hex, generation, stamp])?;
    Ok(())
}

fn insert_file(
    conn: &Connection,
    fingerprint: Option<&Fingerprint>,
    blake3: Option<&str>,
    seen: &[&Seen],
    stamp: &str,
) -> Result<String, Error> {
    let id = Uuid::now_v7().to_string();
    let first = seen[0];
    conn.prepare_cached(
        "INSERT INTO files (id, size_bytes, mtime, fingerprint, fingerprint_scheme, created_at)
         VALUES (?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        id,
        first.size,
        first.mtime,
        fingerprint.map(|print| &print.hex),
        fingerprint.map(|print| &print.scheme),
        stamp
    ])?;
    if let Some(blake3) = blake3 {
        insert_digest(conn, &id, "blake3", blake3, 1, stamp)?;
    }
    let digest_state = if blake3.is_some() {
        "match"
    } else {
        "unverified"
    };
    for one in seen {
        insert_location(conn, &id, one, digest_state, stamp)?;
    }
    Ok(id)
}

fn insert_location(
    conn: &Connection,
    file_id: &str,
    seen: &Seen,
    digest_state: &str,
    stamp: &str,
) -> Result<(), Error> {
    conn.prepare_cached(
        "INSERT INTO file_locations (file_id, backend, uri, first_seen_at, last_seen_at,
           last_size, last_mtime, presence, stat_state, digest_state)
         VALUES (?, 'posix', ?, ?, ?, ?, ?, 'present', 'unchanged', ?)",
    )?
    .execute(params![
        file_id,
        seen.uri,
        stamp,
        stamp,
        seen.size,
        seen.mtime,
        digest_state
    ])?;
    Ok(())
}

fn insert_errors(conn: &Connection, run: &Run, errors: &[(String, String)]) -> Result<(), Error> {
    let mut stmt = conn.prepare(&format!(
        "INSERT INTO {} (run_id, uri, message) VALUES (?, ?, ?)",
        run.kind.errors_table()
    ))?;
    for (uri, message) in errors {
        stmt.execute(params![run.id.to_string(), uri, message])?;
    }
    Ok(())
}

fn resolve_file(conn: &Connection, file: &FileRef) -> Result<String, Error> {
    match file {
        FileRef::Id(id) => {
            let id = id.to_string();
            let exists: bool = conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM files WHERE id = ?)",
                [&id],
                |row| row.get(0),
            )?;
            if exists {
                Ok(id)
            } else {
                Err(Error::FileNotFound(id))
            }
        }
        FileRef::Location { backend, uri } => conn
            .query_row(
                "SELECT file_id FROM file_locations WHERE backend = ? AND uri = ?",
                params![backend, uri],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| Error::FileNotFound(uri.clone())),
    }
}

fn load_file(conn: &Connection, id: &str) -> Result<FileRow, Error> {
    let mut rows = load_files(conn, &[id.to_string()])?;
    rows.pop()
        .ok_or_else(|| Error::FileNotFound(id.to_string()))
}

/// Files with their current digests, locations, and links, in the order of
/// `ids`. The ids go into a temporary table and each detail table is read in
/// one join, so the cost is a few statements however many files are asked for.
/// `CROSS JOIN` keeps the temporary table as the outer loop: it has no
/// statistics, and the planner would otherwise scan a million-row table and
/// probe it. Rows within one file come in each table's key order, which is the
/// order the old per-file queries used.
fn load_files(conn: &Connection, ids: &[String]) -> Result<Vec<FileRow>, Error> {
    conn.execute("DROP TABLE IF EXISTS temp.bpm_result", [])?;
    conn.execute(
        "CREATE TEMP TABLE bpm_result (id TEXT PRIMARY KEY) WITHOUT ROWID",
        [],
    )?;
    {
        let mut insert = conn.prepare("INSERT OR IGNORE INTO temp.bpm_result (id) VALUES (?)")?;
        for id in ids {
            insert.execute([id])?;
        }
    }
    let mut rows = Vec::with_capacity(ids.len());
    let mut stored = Vec::with_capacity(ids.len());
    let mut position = HashMap::with_capacity(ids.len());
    {
        let mut stmt = conn.prepare(
            "SELECT f.id, f.size_bytes, f.mtime, f.fingerprint, f.fingerprint_scheme
             FROM temp.bpm_result r CROSS JOIN files f WHERE f.id = r.id",
        )?;
        let mut found = stmt.query([])?;
        while let Some(row) = found.next()? {
            let id: String = row.get(0)?;
            let fingerprint = match (row.get::<_, Option<String>>(3)?, row.get(4)?) {
                (Some(hex), Some(scheme)) => Some(Fingerprint { scheme, hex }),
                _ => None,
            };
            position.insert(id.clone(), rows.len());
            stored.push(id.clone());
            rows.push(FileRow {
                id: parse_id(&id)?,
                size: row.get(1)?,
                mtime: row.get(2)?,
                fingerprint,
                digests: Vec::new(),
                locations: Vec::new(),
                links: Vec::new(),
            });
        }
    }
    {
        let mut stmt = conn.prepare(
            "SELECT d.algorithm, d.digest, d.source, d.generation, d.current, d.file_id
             FROM temp.bpm_result r CROSS JOIN file_digests d
             WHERE d.file_id = r.id AND d.current = 1",
        )?;
        let mut found = stmt.query([])?;
        while let Some(row) = found.next()? {
            if let Some(&at) = position.get(&row.get::<_, String>(5)?) {
                rows[at].digests.push(digest_row(row)?);
            }
        }
    }
    {
        let mut stmt = conn.prepare(
            "SELECT l.file_id, l.backend, l.uri, l.presence, l.stat_state, l.digest_state, l.last_seen_at
             FROM temp.bpm_result r CROSS JOIN file_locations l
             WHERE l.file_id = r.id",
        )?;
        let mut found = stmt.query([])?;
        while let Some(row) = found.next()? {
            if let Some(&at) = position.get(&row.get::<_, String>(0)?) {
                rows[at].locations.push(LocationRow {
                    backend: row.get(1)?,
                    uri: row.get(2)?,
                    presence: row.get(3)?,
                    stat_state: row.get(4)?,
                    digest_state: row.get(5)?,
                    last_seen_at: row.get(6)?,
                });
            }
        }
    }
    {
        let mut stmt = conn.prepare(
            "SELECT k.file_id, k.node_type, k.node_id, k.role
             FROM temp.bpm_result r CROSS JOIN file_links k
             WHERE k.file_id = r.id",
        )?;
        let mut found = stmt.query([])?;
        while let Some(row) = found.next()? {
            let Some(&at) = position.get(&row.get::<_, String>(0)?) else {
                continue;
            };
            // A link that names a missing or malformed entity is `bpm repair`'s
            // business; a query still shows the rest of the file.
            let (Some(node_type), Ok(node_id)) = (
                NodeType::parse(&row.get::<_, String>(1)?),
                Uuid::parse_str(&row.get::<_, String>(2)?),
            ) else {
                continue;
            };
            rows[at].links.push(LinkRow {
                node_type,
                node_id,
                role: row.get(3)?,
            });
        }
    }
    conn.execute("DROP TABLE temp.bpm_result", [])?;
    // The join returns rows in the temporary table's key order; the caller's
    // order is the order of `ids`.
    let order: HashMap<&str, usize> = ids
        .iter()
        .enumerate()
        .map(|(at, id)| (id.as_str(), at))
        .collect();
    let mut keyed: Vec<(usize, FileRow)> = stored
        .iter()
        .zip(rows)
        .map(|(id, row)| (order[id.as_str()], row))
        .collect();
    keyed.sort_by_key(|(at, _)| *at);
    Ok(keyed.into_iter().map(|(_, row)| row).collect())
}

fn digest_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DigestRow> {
    Ok(DigestRow {
        algorithm: row.get(0)?,
        hex: row.get(1)?,
        source: row.get(2)?,
        generation: row.get(3)?,
        current: row.get::<_, i64>(4)? == 1,
    })
}

fn parse_id(id: &str) -> Result<Uuid, Error> {
    Uuid::parse_str(id).map_err(|err| Error::Message(format!("invalid file id {id}: {err}")))
}
