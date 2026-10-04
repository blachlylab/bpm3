//! Ingest and scan run logs, and the per-run liveness lock.
//!
//! A run holds an exclusive flock on `<catalog>.run-<id>.lock` for its whole
//! life. The catalog write lock is released between batches, so it cannot say
//! whether a run is alive. The flock can: the kernel drops it when the process
//! dies. A writer that finds a run still `running` whose lock it can take marks
//! that run `incomplete`.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use rusqlite::params;
use uuid::Uuid;

use super::{Catalog, begin, timestamp};
use crate::error::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Ingest,
    Scan,
}

impl RunKind {
    fn table(self) -> &'static str {
        match self {
            Self::Ingest => "ingest_runs",
            Self::Scan => "scan_runs",
        }
    }

    pub(super) fn errors_table(self) -> &'static str {
        match self {
            Self::Ingest => "ingest_errors",
            Self::Scan => "scan_errors",
        }
    }
}

/// A live run. Dropping it releases the liveness lock and removes the lock
/// file, so a run that ends without [`Catalog::finish_run`] is found by the
/// next writer and marked `incomplete`.
pub struct Run {
    pub id: Uuid,
    pub kind: RunKind,
    lock: Option<File>,
    lock_path: PathBuf,
}

impl Drop for Run {
    fn drop(&mut self) {
        // Remove the file while the lock is still held, so no other process
        // can take the lock on a file that is about to vanish.
        let _ = fs::remove_file(&self.lock_path);
        self.lock.take();
    }
}

impl Catalog {
    /// Take the liveness lock, then record the run as `running`. In that order,
    /// a `running` row always has a lock file unless its process has died.
    pub fn start_run(
        &mut self,
        kind: RunKind,
        backend: &str,
        root_uri: &str,
    ) -> Result<Run, Error> {
        self.require_write()?;
        let id = Uuid::now_v7();
        let lock_path = lock_path(&self.path, id);
        let lock = create_lock(&lock_path)?;
        let run = Run {
            id,
            kind,
            lock: Some(lock),
            lock_path,
        };
        let stamp = timestamp();
        let tx = begin(&mut self.conn, &self.path)?;
        match kind {
            RunKind::Ingest => tx.execute(
                "INSERT INTO ingest_runs (id, backend, root_uri, started_at, status, files_seen, files_created)
                 VALUES (?, ?, ?, ?, 'running', 0, 0)",
                params![id.to_string(), backend, root_uri, stamp],
            )?,
            RunKind::Scan => tx.execute(
                "INSERT INTO scan_runs (id, backend, root_uri, started_at, status, files_seen)
                 VALUES (?, ?, ?, ?, 'running', 0)",
                params![id.to_string(), backend, root_uri, stamp],
            )?,
        };
        tx.commit()?;
        Ok(run)
    }

    /// Record how the run ended and release its lock. `created` is ignored for
    /// a scan. Afterwards `PRAGMA optimize` refreshes the planner's statistics,
    /// which a run that added many rows can leave stale.
    pub fn finish_run(
        &mut self,
        run: Run,
        complete: bool,
        seen: u64,
        created: u64,
    ) -> Result<(), Error> {
        let status = if complete { "complete" } else { "incomplete" };
        let stamp = timestamp();
        let tx = begin(&mut self.conn, &self.path)?;
        match run.kind {
            RunKind::Ingest => tx.execute(
                "UPDATE ingest_runs SET status = ?, finished_at = ?, files_seen = ?, files_created = ?
                 WHERE id = ?",
                params![status, stamp, seen as i64, created as i64, run.id.to_string()],
            )?,
            RunKind::Scan => tx.execute(
                "UPDATE scan_runs SET status = ?, finished_at = ?, files_seen = ? WHERE id = ?",
                params![status, stamp, seen as i64, run.id.to_string()],
            )?,
        };
        tx.commit()?;
        drop(run);
        // Only statistics; a busy catalog or an old engine is not a failed run.
        let _ = self.conn.execute_batch("PRAGMA optimize");
        Ok(())
    }

    /// Mark `incomplete` every `running` run whose process is gone: its lock
    /// file is missing, or this process can take the lock. Runs on every
    /// writable open.
    pub(super) fn recover_runs(&mut self) -> Result<(), Error> {
        let mut dead = Vec::new();
        for kind in [RunKind::Ingest, RunKind::Scan] {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT id FROM {} WHERE status = 'running'",
                kind.table()
            ))?;
            let ids = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            for id in ids {
                // An id that is not a UUID cannot have a lock file of ours.
                let Ok(parsed) = Uuid::parse_str(&id) else {
                    dead.push((kind, id, None, None));
                    continue;
                };
                let path = lock_path(&self.path, parsed);
                if let Some(lock) = abandoned(&path) {
                    dead.push((kind, id, Some(path), lock));
                }
            }
        }
        if dead.is_empty() {
            return Ok(());
        }
        let stamp = timestamp();
        let tx = begin(&mut self.conn, &self.path)?;
        for (kind, id, _, _) in &dead {
            tx.execute(
                &format!(
                    "UPDATE {} SET status = 'incomplete', finished_at = ? WHERE id = ? AND status = 'running'",
                    kind.table()
                ),
                params![stamp, id],
            )?;
        }
        tx.commit()?;
        for (_, _, path, lock) in dead {
            if let Some(path) = path {
                let _ = fs::remove_file(path);
            }
            drop(lock);
        }
        Ok(())
    }
}

/// `Some` when the run is not alive. The lock, if there was a file to lock, is
/// held until the caller has recorded that.
fn abandoned(path: &Path) -> Option<Option<File>> {
    match File::open(path) {
        Ok(file) => match file.try_lock() {
            Ok(()) => Some(Some(file)),
            Err(_) => None,
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => Some(None),
        // A lock file this process cannot open says nothing either way.
        Err(_) => None,
    }
}

fn create_lock(path: &Path) -> Result<File, Error> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    if file.try_lock().is_err() {
        let _ = fs::remove_file(path);
        return Err(Error::Message(format!("could not lock {}", path.display())));
    }
    Ok(file)
}

pub fn lock_path(catalog: &Path, id: Uuid) -> PathBuf {
    let mut name = catalog.as_os_str().to_owned();
    name.push(format!(".run-{id}.lock"));
    PathBuf::from(name)
}
