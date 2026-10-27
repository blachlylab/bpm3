//! Untrack: remove locations at or under a path, and a file row when its last
//! location goes.
//!
//! Planned against a read snapshot, shown, and planned again inside the write
//! transaction, as a link run is. The write goes ahead only when the second
//! plan is the one that was shown. Bytes on disk are never touched.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use rusqlite::{Connection, params};
use uuid::Uuid;

use super::link::{changed, reach};
use super::{Catalog, begin, timestamp};
use crate::error::Error;
use crate::model::POSIX;

/// What one untrack removes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UntrackPlan {
    /// Locations the operands reached and the filters kept.
    pub selected: usize,
    /// Locations the operands reached and the filters excluded. They stay.
    pub excluded: usize,
    /// Locations removed from files that keep another location.
    pub locations: Vec<String>,
    /// Files whose every location is selected. Each is removed with its rows.
    pub files: Vec<RemovedFile>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedFile {
    pub id: String,
    pub uris: Vec<String>,
    pub links: Vec<FileLink>,
    pub metadata: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLink {
    pub node_type: String,
    pub node_id: String,
    pub role: String,
}

impl UntrackPlan {
    pub fn linked(&self) -> impl Iterator<Item = &RemovedFile> {
        self.files.iter().filter(|file| !file.links.is_empty())
    }

    pub fn links(&self) -> usize {
        self.files.iter().map(|file| file.links.len()).sum()
    }

    pub fn with_metadata(&self) -> usize {
        self.files.iter().filter(|file| file.metadata).count()
    }

    pub fn is_empty(&self) -> bool {
        self.locations.is_empty() && self.files.is_empty()
    }
}

impl Catalog {
    /// Plan an untrack against a read snapshot. `keep` gets each reached
    /// location's URI and its path relative to the operand. Writes nothing.
    pub fn plan_untrack(
        &mut self,
        operands: &[String],
        keep: &dyn Fn(&str, &str) -> bool,
    ) -> Result<UntrackPlan, Error> {
        let snapshot = self.conn.transaction()?;
        plan(&snapshot, operands, keep)
    }

    /// Plan again under the write lock and, if that is the plan that was
    /// shown, apply it. Returns the run id, or `None` when there was nothing
    /// to write.
    pub fn apply_untrack(
        &mut self,
        operands: &[String],
        keep: &dyn Fn(&str, &str) -> bool,
        shown: &UntrackPlan,
        cascade: bool,
        command: &str,
    ) -> Result<Option<Uuid>, Error> {
        self.require_write()?;
        let tx = begin(&mut self.conn, &self.path)?;
        let plan = plan(&tx, operands, keep)?;
        if plan != *shown {
            return Err(changed());
        }
        if plan.linked().next().is_some() && !cascade {
            return Err(linked_error(&plan));
        }
        if plan.is_empty() {
            return Ok(None);
        }
        for uri in &plan.locations {
            tx.execute(
                "DELETE FROM file_locations WHERE backend = ? AND uri = ?",
                params![POSIX, uri],
            )?;
        }
        for file in &plan.files {
            for table in [
                "file_links",
                "file_metadata",
                "file_digests",
                "file_locations",
            ] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE file_id = ?"),
                    [&file.id],
                )?;
            }
            tx.execute("DELETE FROM files WHERE id = ?", [&file.id])?;
        }
        let run = Uuid::now_v7();
        tx.execute(
            "INSERT INTO untrack_runs
               (id, command, applied_at, locations_removed, files_removed, links_removed)
             VALUES (?, ?, ?, ?, ?, ?)",
            params![
                run.to_string(),
                command,
                timestamp(),
                plan.locations.len() as i64,
                plan.files.len() as i64,
                plan.links() as i64
            ],
        )?;
        tx.commit()?;
        Ok(Some(run))
    }
}

/// The refusal for linked files that would lose their last location.
pub fn linked_error(plan: &UntrackPlan) -> Error {
    let count = plan.linked().count();
    Error::Message(format!(
        "{count} linked file(s) would lose their last location; nothing was written. \
         Unlink them first, or pass --cascade to remove their links"
    ))
}

fn plan(
    conn: &Connection,
    operands: &[String],
    keep: &dyn Fn(&str, &str) -> bool,
) -> Result<UntrackPlan, Error> {
    let mut out = UntrackPlan::default();
    let mut missing = Vec::new();
    // file id → the selected URIs of that file
    let mut selected: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut kept: HashSet<String> = HashSet::new();
    let mut excluded: HashSet<String> = HashSet::new();
    for operand in operands {
        let found = reach(conn, POSIX, operand)?;
        if found.is_empty() {
            missing.push(operand.as_str());
        }
        for source in found {
            if !keep(&source.uri, &source.relpath) {
                excluded.insert(source.uri);
            } else if kept.insert(source.uri.clone()) {
                selected
                    .entry(source.file_id)
                    .or_default()
                    .insert(source.uri);
            }
        }
    }
    if !missing.is_empty() {
        return Err(Error::Message(format!(
            "no catalog location at or under {}",
            missing.join(", ")
        )));
    }
    out.selected = kept.len();
    // A location two operands reach, one keeping it, is selected.
    out.excluded = excluded.difference(&kept).count();
    let mut all = conn.prepare_cached(
        "SELECT backend, uri FROM file_locations WHERE file_id = ? ORDER BY backend, uri",
    )?;
    let mut links = conn.prepare_cached(
        "SELECT node_type, node_id, role FROM file_links WHERE file_id = ?
         ORDER BY node_type, node_id",
    )?;
    let mut metadata =
        conn.prepare_cached("SELECT EXISTS (SELECT 1 FROM file_metadata WHERE file_id = ?)")?;
    for (id, uris) in selected {
        let locations = all
            .query_map([&id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let keeps_one = locations
            .iter()
            .any(|(backend, uri)| backend != POSIX || !uris.contains(uri));
        if keeps_one {
            out.locations.extend(uris);
            continue;
        }
        let links = links
            .query_map([&id], |row| {
                Ok(FileLink {
                    node_type: row.get(0)?,
                    node_id: row.get(1)?,
                    role: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let metadata = metadata.query_row([&id], |row| row.get(0))?;
        out.files.push(RemovedFile {
            id,
            uris: uris.into_iter().collect(),
            links,
            metadata,
        });
    }
    out.locations.sort();
    Ok(out)
}
