//! SQLite catalog: init, the entity tree, metadata, and read-only SQL.
//!
//! Callers outside this module do not see SQL. The catalog runs in WAL mode, so
//! a [`Catalog`] opened for reading sees the last committed state while another
//! process writes. It does not migrate and never takes the write lock. A write
//! takes the lock with `BEGIN IMMEDIATE` for the length of one transaction.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use rusqlite::backup::{Backup, StepResult};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, ErrorCode, OpenFlags, Transaction, TransactionBehavior, ffi, params};
use uuid::Uuid;

use crate::error::Error;
use crate::migrate::{self, SchemaState};
use crate::model::{
    Address, EntityRow, NodeType, Selector, parse_address, validate_meta_token, validate_name,
};
use crate::perms;

/// Filters for `bpm query entities`. An absent `under` means the whole catalog.
pub struct EntityQuery {
    pub under: Option<String>,
    pub node_type: Option<NodeType>,
    pub wheres: Vec<Selector>,
}

pub enum SqlOutcome {
    Rows {
        columns: Vec<String>,
        rows: Vec<Vec<String>>,
    },
    Executed {
        rows_affected: usize,
    },
}

/// How long a writer waits for another process's write transaction before it
/// reports the catalog as busy.
const BUSY_WAIT: Duration = Duration::from_secs(3);

pub struct Catalog {
    conn: Connection,
    writable: bool,
    path: PathBuf,
}

struct NodeRec {
    node_type: NodeType,
    id: Uuid,
    parent_id: Option<Uuid>,
    name: Option<String>,
    metadata: BTreeMap<String, String>,
}

struct Index {
    nodes: Vec<NodeRec>,
    by_id: HashMap<Uuid, usize>,
    children: HashMap<Uuid, Vec<Uuid>>,
}

impl Catalog {
    pub fn open_read(path: &Path) -> Result<Self, Error> {
        require_file(path)?;
        let conn = readonly(path)?;
        migrate::ensure_current(&conn)?;
        Ok(Self {
            conn,
            writable: false,
            path: path.to_path_buf(),
        })
    }

    pub fn open_write(path: &Path) -> Result<Self, Error> {
        require_file(path)?;
        // Refuse a newer, corrupt, or foreign file without opening it for write,
        // so the refusal does not rewrite the file.
        {
            let peek = readonly(path)?;
            match migrate::inspect(&peek)? {
                SchemaState::Version(found) if found > migrate::latest() => {
                    return Err(Error::SchemaNewer {
                        found,
                        supported: migrate::latest(),
                    });
                }
                SchemaState::Corrupt => return Err(Error::CorruptSchema),
                SchemaState::Foreign => return Err(Error::NotACatalog(path.to_path_buf())),
                _ => {}
            }
        }
        let mut conn = writable(path)?;
        migrate::apply(&mut conn).map_err(|err| busy(err, path))?;
        Ok(Self {
            conn,
            writable: true,
            path: path.to_path_buf(),
        })
    }

    /// Create a catalog at `path`. A file that is already there is left untouched
    /// unless `force` replaces it.
    pub fn init(path: &Path, force: bool) -> Result<(), Error> {
        create_parents(path)?;
        if force && path.exists() {
            return replace(path);
        }
        // Exclusive creation is the existence check, so of two overlapping inits
        // exactly one owns the new file and the other reports that it exists.
        match perms::create_user_file(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(Error::AlreadyExists(path.to_path_buf()));
            }
            Err(err) => return Err(err.into()),
        }
        // The file is this call's own, so a failure removes only what it created.
        let result = writable(path).and_then(|mut conn| build(&mut conn, path));
        if result.is_err() {
            remove_catalog_files(path);
        }
        result
    }

    /// Fail with [`Error::Inconsistent`] if any entity's parent is missing.
    /// This is `PRAGMA foreign_key_check` on the node tables only, which is what
    /// every tree walk depends on. The tree is small next to the file tables, so
    /// it is cheap enough to run before each command. `bpm repair` checks
    /// everything.
    pub fn check_tree(&self) -> Result<(), Error> {
        let sql = format!(
            "SELECT {}",
            NODE_PARENTS
                .iter()
                .map(|(table, _, _)| {
                    format!("(SELECT COUNT(*) FROM pragma_foreign_key_check('{table}'))")
                })
                .collect::<Vec<_>>()
                .join(" + ")
        );
        let orphans: i64 = self.conn.query_row(&sql, [], |row| row.get(0))?;
        if orphans > 0 {
            let entities = if orphans == 1 {
                "entity has"
            } else {
                "entities have"
            };
            return Err(Error::Inconsistent(format!(
                "{orphans} {entities} no parent"
            )));
        }
        Ok(())
    }

    /// Every row that breaks a declared foreign key or a `(node_type, node_id)`
    /// reference. Does not write.
    pub fn problems(&mut self) -> Result<Vec<Problem>, Error> {
        let snapshot = self.conn.transaction()?;
        find_problems(&snapshot)
    }

    /// Remove every row [`Catalog::problems`] reports, in one write transaction.
    /// An entity whose parent is missing goes with its subtree, metadata, and
    /// links, the way `delete --cascade` would remove it. File rows are kept.
    /// Returns what was found and how many descendants went with it.
    pub fn repair(&mut self) -> Result<(Vec<Problem>, usize), Error> {
        self.require_write()?;
        let tx = begin(&mut self.conn, &self.path)?;
        let found = find_problems(&tx)?;
        if found.is_empty() {
            return Ok((found, 0));
        }
        let index = load_index(&tx)?;
        let mut doomed = Vec::new();
        let mut seen = HashSet::new();
        for problem in found.iter().filter(|problem| is_node_table(problem.table)) {
            for key in &problem.keys {
                let id = Uuid::parse_str(key)
                    .map_err(|err| Error::Message(format!("invalid entity id {key}: {err}")))?;
                if seen.insert(id) {
                    doomed.push(id);
                }
            }
        }
        let orphans = doomed.len();
        for id in doomed.clone() {
            collect_descendants_seen(id, &index, &mut doomed, &mut seen);
        }
        let descendants = doomed.len() - orphans;
        delete_ids(&tx, &doomed)?;
        for check in checks().iter().filter(|check| !is_node_table(check.table)) {
            tx.execute(
                &format!("DELETE FROM {} WHERE {}", check.table, check.predicate),
                [],
            )?;
        }
        let left: i64 =
            tx.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        if left > 0 || !find_problems(&tx)?.is_empty() {
            return Err(Error::Message(
                "repair did not remove every problem; the catalog is unchanged".into(),
            ));
        }
        tx.commit()?;
        Ok((found, descendants))
    }

    pub fn create(
        &mut self,
        node_type: NodeType,
        name: Option<&str>,
        parent: Option<&str>,
    ) -> Result<String, Error> {
        self.require_write()?;
        if let Some(name) = name {
            if !node_type.takes_name() {
                return Err(Error::NameNotAllowed(node_type.slug()));
            }
            validate_name(name)?;
        } else if node_type.takes_name() {
            return Err(Error::NameRequired);
        }
        if parent.is_some() && node_type == NodeType::Program {
            return Err(Error::IllegalParent);
        }
        if parent.is_none() && node_type != NodeType::Program {
            return Err(Error::ParentRequired);
        }

        let id = Uuid::now_v7();
        let stamp = timestamp();
        let tx = begin(&mut self.conn, &self.path)?;
        let index = load_index(&tx)?;
        let parent_id = if let Some(parent) = parent {
            let expected = node_type.parent_type().ok_or(Error::IllegalParent)?;
            let parent_node = resolve_one(&index, parent)?;
            if parent_node.node_type != expected {
                return Err(Error::IllegalParent);
            }
            Some(parent_node.id)
        } else {
            None
        };
        if let Some(name) = name {
            if sibling_taken(&tx, node_type, parent_id, name, None)? {
                return Err(Error::SiblingNameTaken);
            }
        }
        insert_node(&tx, node_type, id, parent_id, name, &stamp).map_err(map_write)?;
        let printed = match node_type {
            NodeType::Program => format!("/{}", name.unwrap_or_default()),
            NodeType::Project => {
                let parent = index.get(parent_id.unwrap());
                format!(
                    "/{}/{}",
                    parent.name.as_deref().unwrap_or_default(),
                    name.unwrap_or_default()
                )
            }
            _ => id.to_string(),
        };
        tx.commit()?;
        Ok(printed)
    }

    pub fn rename(&mut self, target: &str, new_name: &str) -> Result<(), Error> {
        self.require_write()?;
        validate_name(new_name)?;
        let stamp = timestamp();
        let tx = begin(&mut self.conn, &self.path)?;
        let index = load_index(&tx)?;
        let node = resolve_one(&index, target)?;
        if !node.node_type.takes_name() {
            return Err(Error::NotRenameable);
        }
        if sibling_taken(&tx, node.node_type, node.parent_id, new_name, Some(node.id))? {
            return Err(Error::SiblingNameTaken);
        }
        let table = table_name(node.node_type);
        let sql = format!("UPDATE {table} SET name = ?, updated_at = ? WHERE id = ?");
        tx.execute(&sql, params![new_name, stamp, node.id.to_string()])
            .map_err(map_write)?;
        tx.commit()?;
        Ok(())
    }

    pub fn reparent(&mut self, target: &str, new_parent: &str) -> Result<(), Error> {
        self.require_write()?;
        let stamp = timestamp();
        let tx = begin(&mut self.conn, &self.path)?;
        let index = load_index(&tx)?;
        let node = resolve_one(&index, target)?;
        let expected = node.node_type.parent_type().ok_or(Error::ProgramNoParent)?;
        let parent = resolve_one(&index, new_parent)?;
        if parent.node_type != expected {
            return Err(Error::IllegalParent);
        }
        let column = parent_column(node.node_type).expect("type has a parent");
        let table = table_name(node.node_type);
        let sql = format!("UPDATE {table} SET {column} = ?, updated_at = ? WHERE id = ?");
        tx.execute(
            &sql,
            params![parent.id.to_string(), stamp, node.id.to_string()],
        )
        .map_err(map_write)?;
        tx.commit()?;
        Ok(())
    }

    pub fn delete(&mut self, target: &str, cascade: bool) -> Result<(), Error> {
        self.require_write()?;
        let tx = begin(&mut self.conn, &self.path)?;
        let index = load_index(&tx)?;
        let node = resolve_one(&index, target)?;
        let mut ids = vec![node.id];
        collect_descendants(node.id, &index, &mut ids);
        if !cascade {
            let descendants = ids.len() - 1;
            let links: i64 = tx.query_row(
                "SELECT COUNT(*) FROM file_links WHERE node_type = ? AND node_id = ?",
                params![node.node_type.slug(), node.id.to_string()],
                |row| row.get(0),
            )?;
            if descendants > 0 || links > 0 {
                return Err(Error::HasDependents);
            }
            ids.truncate(1);
        }
        delete_ids(&tx, &ids)?;
        tx.commit()?;
        Ok(())
    }

    pub fn meta_set(&mut self, target: &str, key: &str, value: &str) -> Result<(), Error> {
        self.require_write()?;
        validate_meta_token(key)?;
        validate_meta_token(value)?;
        let stamp = timestamp();
        let tx = begin(&mut self.conn, &self.path)?;
        let index = load_index(&tx)?;
        let node = resolve_one(&index, target)?;
        tx.execute(
            "INSERT INTO entity_metadata (node_type, node_id, key, value, updated_at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (node_type, node_id, key) DO UPDATE SET
               value = excluded.value,
               updated_at = excluded.updated_at",
            params![
                node.node_type.slug(),
                node.id.to_string(),
                key,
                value,
                stamp
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn meta_get(
        &mut self,
        target: &str,
        key: Option<&str>,
    ) -> Result<Vec<(String, String)>, Error> {
        // One read transaction, so the nodes and their metadata are one snapshot.
        let snapshot = self.conn.transaction()?;
        let index = load_index(&snapshot)?;
        let node = resolve_one(&index, target)?;
        if let Some(key) = key {
            validate_meta_token(key)?;
            let value = node
                .metadata
                .get(key)
                .ok_or_else(|| Error::MetaMissing(key.to_string()))?;
            Ok(vec![(key.to_string(), value.clone())])
        } else {
            Ok(node
                .metadata
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect())
        }
    }

    pub fn meta_unset(&mut self, target: &str, key: &str) -> Result<(), Error> {
        self.require_write()?;
        validate_meta_token(key)?;
        let tx = begin(&mut self.conn, &self.path)?;
        let index = load_index(&tx)?;
        let node = resolve_one(&index, target)?;
        let changed = tx.execute(
            "DELETE FROM entity_metadata WHERE node_type = ? AND node_id = ? AND key = ?",
            params![node.node_type.slug(), node.id.to_string(), key],
        )?;
        if changed == 0 {
            return Err(Error::MetaMissing(key.to_string()));
        }
        tx.commit()?;
        Ok(())
    }

    pub fn query_entities(&mut self, query: &EntityQuery) -> Result<Vec<EntityRow>, Error> {
        // One read transaction, so the nodes and their metadata are one snapshot.
        let snapshot = self.conn.transaction()?;
        let index = load_index(&snapshot)?;
        let mut ids: Vec<Uuid> = if let Some(under) = &query.under {
            let roots = resolve_all(&index, under)?;
            let mut gathered = Vec::new();
            let mut seen = HashSet::new();
            for root in roots {
                if seen.insert(root) {
                    gathered.push(root);
                }
                collect_descendants_seen(root, &index, &mut gathered, &mut seen);
            }
            gathered
        } else {
            index.nodes.iter().map(|node| node.id).collect()
        };
        if !query.wheres.is_empty() {
            ids.retain(|id| {
                let node = index.get(*id);
                query
                    .wheres
                    .iter()
                    .all(|selector| selector.matches(&node.metadata))
            });
        }
        if let Some(node_type) = query.node_type {
            ids.retain(|id| index.get(*id).node_type == node_type);
        }
        let mut rows = ids
            .into_iter()
            .map(|id| {
                let node = index.get(id);
                Ok(EntityRow {
                    node_type: node.node_type,
                    id: node.id,
                    path: path_of(node, &index)?,
                    name: node.name.clone(),
                    metadata: node.metadata.clone(),
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        rows.sort_by(|a, b| {
            a.node_type
                .rank()
                .cmp(&b.node_type.rank())
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(rows)
    }

    /// Run one statement. A read-only catalog rejects any write in the engine.
    /// A writable one runs the statement in a write transaction with foreign
    /// keys enforced, and rolls it back if the engine rejects it.
    pub fn run_sql(&mut self, sql: &str) -> Result<SqlOutcome, Error> {
        let sql = sql.trim().trim_end_matches(';').trim();
        if sql.is_empty() {
            return Err(Error::Message("SQL statement is empty".into()));
        }
        if self.writable {
            let tx = begin(&mut self.conn, &self.path)?;
            let outcome = run_statement(&tx, sql)?;
            tx.commit()?;
            Ok(outcome)
        } else {
            run_statement(&self.conn, sql)
        }
    }

    fn require_write(&self) -> Result<(), Error> {
        if self.writable {
            Ok(())
        } else {
            Err(Error::Message("catalog is open read-only".into()))
        }
    }
}

pub fn resolve_catalog_path(flag: Option<&Path>) -> Result<PathBuf, Error> {
    if let Some(path) = flag {
        if !path.as_os_str().is_empty() {
            return Ok(path.to_path_buf());
        }
    }
    if let Some(from_env) = env::var_os("BPM_CATALOG") {
        if !from_env.is_empty() {
            return Ok(PathBuf::from(from_env));
        }
    }
    default_catalog_path()
}

pub fn default_catalog_path() -> Result<PathBuf, Error> {
    Ok(home_dir()?.join(".bpm").join("default.db"))
}

pub fn home_dir() -> Result<PathBuf, Error> {
    match env::var_os("HOME") {
        Some(home) if !home.is_empty() => Ok(PathBuf::from(home)),
        _ => Err(Error::Message("HOME is not set".into())),
    }
}

fn require_file(path: &Path) -> Result<(), Error> {
    if path.is_file() {
        Ok(())
    } else {
        Err(Error::MissingCatalog {
            path: path.to_path_buf(),
        })
    }
}

fn readonly(path: &Path) -> Result<Connection, Error> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(BUSY_WAIT)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(conn)
}

/// Open an existing file for write. Whether foreign keys are enforced by
/// default depends on how SQLite was built, so every connection turns them on.
fn writable(path: &Path) -> Result<Connection, Error> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(BUSY_WAIT)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    let mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(Error::Message(format!(
            "catalog could not switch to WAL mode (journal_mode is {mode})"
        )));
    }
    conn.pragma_update(None, "synchronous", "FULL")?;
    Ok(conn)
}

/// Start a write transaction. `BEGIN IMMEDIATE` takes the catalog write lock up
/// front, so a second writer waits up to [`BUSY_WAIT`] here and then fails,
/// instead of failing halfway through its work.
fn begin<'a>(conn: &'a mut Connection, path: &Path) -> Result<Transaction<'a>, Error> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|err| busy(err.into(), path))
}

/// Report SQLite's busy and locked errors as the typed busy error.
fn busy(err: Error, path: &Path) -> Error {
    match &err {
        Error::Sqlite(inner)
            if matches!(
                inner.sqlite_error_code(),
                Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
            ) =>
        {
            Error::Busy(path.to_path_buf())
        }
        _ => err,
    }
}

/// Stamp an empty database as a BPM catalog: the application id, every
/// migration, and the catalog's identity.
fn build(conn: &mut Connection, path: &Path) -> Result<(), Error> {
    conn.pragma_update(None, "application_id", migrate::APPLICATION_ID)?;
    migrate::apply(conn).map_err(|err| busy(err, path))?;
    insert_identity(conn, path)
}

/// `init --force` over an existing file. The new catalog is built in memory and
/// copied over the old one by SQLite's backup API in a single write
/// transaction. A catalog another process is writing is refused as busy, no
/// writer can commit into a file that is about to be replaced, and any failure
/// leaves the old catalog as it was.
fn replace(path: &Path) -> Result<(), Error> {
    let mut fresh = Connection::open_in_memory()?;
    build(&mut fresh, path)?;
    let mut target = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    target.busy_timeout(BUSY_WAIT)?;
    if let Err(err) = copy_into(&fresh, &mut target, path) {
        drop(target);
        return match err {
            Error::Sqlite(inner) if inner.sqlite_error_code() == Some(ErrorCode::NotADatabase) => {
                replace_foreign(path, &fresh)
            }
            other => Err(other),
        };
    }
    drop(target);
    perms::set_user_file(path)?;
    // Reopening for write confirms the replaced catalog is in WAL mode.
    writable(path)?;
    Ok(())
}

/// `init --force` over a file that is not a SQLite database. No SQLite process
/// can be using it, so the catalog is built beside it and renamed over it.
fn replace_foreign(path: &Path, fresh: &Connection) -> Result<(), Error> {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".init-{}", Uuid::now_v7()));
    let staged = PathBuf::from(name);
    let result = (|| -> Result<(), Error> {
        perms::create_user_file(&staged)?;
        let mut conn = writable(&staged)?;
        copy_into(fresh, &mut conn, &staged)?;
        drop(conn);
        remove_sidecars(path);
        fs::rename(&staged, path)?;
        Ok(())
    })();
    if result.is_err() {
        remove_catalog_files(&staged);
    }
    result
}

fn copy_into(from: &Connection, to: &mut Connection, path: &Path) -> Result<(), Error> {
    let backup = Backup::new(from, to)?;
    match backup.step(-1)? {
        StepResult::Done => Ok(()),
        StepResult::Busy | StepResult::Locked => Err(Error::Busy(path.to_path_buf())),
        _ => Err(Error::Message("catalog copy did not finish".into())),
    }
}

fn create_parents(path: &Path) -> Result<(), Error> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    let bpm = home_dir().ok().map(|home| home.join(".bpm"));
    let bpm_existed = bpm.as_ref().is_some_and(|dir| dir.exists());
    fs::create_dir_all(parent)?;
    if let Some(bpm) = bpm {
        if !bpm_existed && bpm.is_dir() {
            perms::set_user_dir(&bpm)?;
        }
    }
    Ok(())
}

fn remove_catalog_files(path: &Path) {
    let _ = fs::remove_file(path);
    remove_sidecars(path);
}

fn remove_sidecars(path: &Path) {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sibling = path.as_os_str().to_owned();
        sibling.push(suffix);
        let _ = fs::remove_file(sibling);
    }
}

fn insert_identity(conn: &mut Connection, path: &Path) -> Result<(), Error> {
    let id = Uuid::now_v7().to_string();
    let label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "catalog".into());
    let created = timestamp();
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute(
        "INSERT INTO catalog_meta (key, value) VALUES ('catalog_id', ?)",
        params![id],
    )?;
    tx.execute(
        "INSERT INTO catalog_meta (key, value) VALUES ('label', ?)",
        params![label],
    )?;
    tx.execute(
        "INSERT INTO catalog_meta (key, value) VALUES ('created_at', ?)",
        params![created],
    )?;
    tx.commit()?;
    Ok(())
}

/// UTC, ISO 8601, millisecond precision, `Z` suffix. Sorts as it reads.
fn timestamp() -> String {
    Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

fn table_name(node_type: NodeType) -> &'static str {
    match node_type {
        NodeType::Program => "programs",
        NodeType::Project => "projects",
        NodeType::Case => "cases",
        NodeType::Sample => "samples",
        NodeType::RawData => "raw_data",
        NodeType::Analysis => "analyses",
    }
}

fn parent_column(node_type: NodeType) -> Option<&'static str> {
    match node_type {
        NodeType::Program => None,
        NodeType::Project => Some("program_id"),
        NodeType::Case => Some("project_id"),
        NodeType::Sample => Some("case_id"),
        NodeType::RawData => Some("sample_id"),
        NodeType::Analysis => Some("raw_data_id"),
    }
}

fn insert_node(
    conn: &Connection,
    node_type: NodeType,
    id: Uuid,
    parent_id: Option<Uuid>,
    name: Option<&str>,
    stamp: &str,
) -> rusqlite::Result<()> {
    let id = id.to_string();
    match node_type {
        NodeType::Program => {
            conn.execute(
                "INSERT INTO programs (id, name, created_at, updated_at)
                 VALUES (?, ?, ?, ?)",
                params![id, name.unwrap_or_default(), stamp, stamp],
            )?;
        }
        NodeType::Project => {
            conn.execute(
                "INSERT INTO projects (id, program_id, name, created_at, updated_at)
                 VALUES (?, ?, ?, ?, ?)",
                params![
                    id,
                    parent_id.unwrap().to_string(),
                    name.unwrap_or_default(),
                    stamp,
                    stamp
                ],
            )?;
        }
        NodeType::Case => {
            conn.execute(
                "INSERT INTO cases (id, project_id, created_at, updated_at)
                 VALUES (?, ?, ?, ?)",
                params![id, parent_id.unwrap().to_string(), stamp, stamp],
            )?;
        }
        NodeType::Sample => {
            conn.execute(
                "INSERT INTO samples (id, case_id, created_at, updated_at)
                 VALUES (?, ?, ?, ?)",
                params![id, parent_id.unwrap().to_string(), stamp, stamp],
            )?;
        }
        NodeType::RawData => {
            conn.execute(
                "INSERT INTO raw_data (id, sample_id, created_at, updated_at)
                 VALUES (?, ?, ?, ?)",
                params![id, parent_id.unwrap().to_string(), stamp, stamp],
            )?;
        }
        NodeType::Analysis => {
            conn.execute(
                "INSERT INTO analyses (id, raw_data_id, created_at, updated_at)
                 VALUES (?, ?, ?, ?)",
                params![id, parent_id.unwrap().to_string(), stamp, stamp],
            )?;
        }
    }
    Ok(())
}

fn sibling_taken(
    conn: &Connection,
    node_type: NodeType,
    parent_id: Option<Uuid>,
    name: &str,
    except: Option<Uuid>,
) -> Result<bool, Error> {
    let count: i64 = match node_type {
        NodeType::Program => conn.query_row(
            "SELECT COUNT(*) FROM programs WHERE name = ? AND id <> ?",
            params![name, except.map(|id| id.to_string()).unwrap_or_default()],
            |row| row.get(0),
        )?,
        NodeType::Project => conn.query_row(
            "SELECT COUNT(*) FROM projects WHERE program_id = ? AND name = ? AND id <> ?",
            params![
                parent_id.map(|id| id.to_string()).unwrap_or_default(),
                name,
                except.map(|id| id.to_string()).unwrap_or_default()
            ],
            |row| row.get(0),
        )?,
        _ => 0,
    };
    Ok(count > 0)
}

fn delete_ids(conn: &Connection, ids: &[Uuid]) -> Result<(), Error> {
    conn.execute("DROP TABLE IF EXISTS bpm_doomed", [])?;
    conn.execute("CREATE TEMP TABLE bpm_doomed (id TEXT PRIMARY KEY)", [])?;
    for id in ids {
        conn.execute(
            "INSERT INTO bpm_doomed (id) VALUES (?)",
            params![id.to_string()],
        )?;
    }
    conn.execute(
        "DELETE FROM file_links WHERE node_id IN (SELECT id FROM bpm_doomed)",
        [],
    )?;
    conn.execute(
        "DELETE FROM entity_metadata WHERE node_id IN (SELECT id FROM bpm_doomed)",
        [],
    )?;
    for table in [
        "analyses", "raw_data", "samples", "cases", "projects", "programs",
    ] {
        conn.execute(
            &format!("DELETE FROM {table} WHERE id IN (SELECT id FROM bpm_doomed)"),
            [],
        )?;
    }
    conn.execute("DROP TABLE bpm_doomed", [])?;
    Ok(())
}

/// Node tables with a parent: (table, parent column, parent table).
const NODE_PARENTS: [(&str, &str, &str); 5] = [
    ("projects", "program_id", "programs"),
    ("cases", "project_id", "projects"),
    ("samples", "case_id", "cases"),
    ("raw_data", "sample_id", "samples"),
    ("analyses", "raw_data_id", "raw_data"),
];

/// Rows in one table that break the catalog's integrity.
pub struct Problem {
    pub table: &'static str,
    pub issue: &'static str,
    /// The rows, by their key. For a node table the key is the entity UUID.
    pub keys: Vec<String>,
}

struct Check {
    table: &'static str,
    issue: &'static str,
    key: &'static str,
    predicate: String,
}

/// Every integrity rule, in repair order: entities first (removing an orphan
/// subtree also removes its metadata and links), then rows that name a missing
/// entity, file, or run. The node and file rules are the declared foreign
/// keys. `(node_type, node_id)` is not a foreign key, so it is checked here.
fn checks() -> Vec<Check> {
    let mut checks: Vec<Check> = NODE_PARENTS
        .iter()
        .map(|(table, column, parent)| Check {
            table,
            issue: "parent entity is missing",
            key: "id",
            predicate: format!(
                "NOT EXISTS (SELECT 1 FROM {parent} WHERE {parent}.id = {table}.{column})"
            ),
        })
        .collect();
    for (table, key) in [
        (
            "entity_metadata",
            "node_type || ' ' || node_id || ' ' || key",
        ),
        (
            "file_links",
            "file_id || ' ' || node_type || ' ' || node_id",
        ),
    ] {
        checks.push(Check {
            table,
            issue: "entity is missing",
            key,
            predicate: format!(
                "NOT EXISTS (SELECT 1 FROM entities WHERE entities.node_type = {table}.node_type AND entities.id = {table}.node_id)"
            ),
        });
    }
    for (table, key) in [
        ("file_metadata", "file_id || ' ' || key"),
        (
            "file_digests",
            "file_id || ' ' || algorithm || ' ' || generation",
        ),
        ("file_locations", "backend || ':' || uri"),
        (
            "file_links",
            "file_id || ' ' || node_type || ' ' || node_id",
        ),
    ] {
        checks.push(Check {
            table,
            issue: "file is missing",
            key,
            predicate: format!("NOT EXISTS (SELECT 1 FROM files WHERE files.id = {table}.file_id)"),
        });
    }
    for (table, runs) in [
        ("ingest_errors", "ingest_runs"),
        ("scan_errors", "scan_runs"),
    ] {
        checks.push(Check {
            table,
            issue: "run is missing",
            key: "run_id || ' ' || uri",
            predicate: format!(
                "NOT EXISTS (SELECT 1 FROM {runs} WHERE {runs}.id = {table}.run_id)"
            ),
        });
    }
    checks
}

fn is_node_table(table: &str) -> bool {
    NODE_PARENTS
        .iter()
        .any(|(node_table, _, _)| *node_table == table)
}

fn find_problems(conn: &Connection) -> Result<Vec<Problem>, Error> {
    let mut found = Vec::new();
    for check in checks() {
        let mut stmt = conn.prepare(&format!(
            "SELECT {} FROM {} WHERE {} ORDER BY 1",
            check.key, check.table, check.predicate
        ))?;
        let keys = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        if !keys.is_empty() {
            found.push(Problem {
                table: check.table,
                issue: check.issue,
                keys,
            });
        }
    }
    Ok(found)
}

fn load_index(conn: &Connection) -> Result<Index, Error> {
    let mut nodes = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT node_type, id, parent_id, name FROM entities")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (node_type, id, parent_id, name) = row?;
            let node_type = NodeType::parse(&node_type)
                .ok_or_else(|| Error::Message(format!("unknown node type {node_type}")))?;
            let id = Uuid::parse_str(&id)
                .map_err(|err| Error::Message(format!("invalid entity id {id}: {err}")))?;
            // A parent id that is not a UUID cannot name an entity, so it is a
            // missing parent like any other: the startup check and `bpm repair`
            // report it, and `reparent` or `delete` can still reach the entity.
            let parent_id = parent_id.and_then(|value| Uuid::parse_str(&value).ok());
            nodes.push(NodeRec {
                node_type,
                id,
                parent_id,
                name,
                metadata: BTreeMap::new(),
            });
        }
    }
    {
        let mut stmt = conn.prepare("SELECT node_id, key, value FROM entity_metadata")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let positions: HashMap<Uuid, usize> = nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id, index))
            .collect();
        for row in rows {
            let (node_id, key, value) = row?;
            let Ok(node_id) = Uuid::parse_str(&node_id) else {
                continue;
            };
            if let Some(position) = positions.get(&node_id) {
                nodes[*position].metadata.insert(key, value);
            }
        }
    }
    let mut by_id = HashMap::new();
    let mut children: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for (position, node) in nodes.iter().enumerate() {
        by_id.insert(node.id, position);
        if let Some(parent) = node.parent_id {
            children.entry(parent).or_default().push(node.id);
        }
    }
    Ok(Index {
        nodes,
        by_id,
        children,
    })
}

impl Index {
    /// For ids taken from this index. An id from a row's parent column may be
    /// missing in an inconsistent catalog; use [`Index::lookup`] for those.
    fn get(&self, id: Uuid) -> &NodeRec {
        &self.nodes[self.by_id[&id]]
    }

    fn lookup(&self, id: Uuid) -> Option<&NodeRec> {
        self.by_id.get(&id).map(|position| &self.nodes[*position])
    }
}

/// Selectors after an anchor search that anchor's descendants. The anchor itself
/// is not required to match the first selector. Each later selector searches the
/// current matches and their descendants.
fn resolve_all(index: &Index, address: &str) -> Result<Vec<Uuid>, Error> {
    let parsed = parse_address(address)?;
    let (anchors, selectors) = match parsed {
        Address::Path {
            program,
            project,
            selectors,
        } => {
            let Some(program_node) = index.nodes.iter().find(|node| {
                node.node_type == NodeType::Program
                    && node.name.as_deref() == Some(program.as_str())
            }) else {
                return Ok(Vec::new());
            };
            let anchor = if let Some(project_name) = project {
                match index.nodes.iter().find(|node| {
                    node.node_type == NodeType::Project
                        && node.parent_id == Some(program_node.id)
                        && node.name.as_deref() == Some(project_name.as_str())
                }) {
                    Some(project_node) => project_node.id,
                    None => return Ok(Vec::new()),
                }
            } else {
                program_node.id
            };
            (vec![anchor], selectors)
        }
        Address::Id { id, selectors } => {
            if !index.by_id.contains_key(&id) {
                return Ok(Vec::new());
            }
            (vec![id], selectors)
        }
    };
    if selectors.is_empty() {
        return Ok(anchors);
    }
    let mut current = anchors;
    for (ordinal, selector) in selectors.iter().enumerate() {
        let mut seen = HashSet::new();
        let mut candidates = Vec::new();
        for id in &current {
            if ordinal > 0 && seen.insert(*id) {
                candidates.push(*id);
            }
            collect_descendants_seen(*id, index, &mut candidates, &mut seen);
        }
        current = candidates
            .into_iter()
            .filter(|id| selector.matches(&index.get(*id).metadata))
            .collect();
    }
    Ok(current)
}

fn resolve_one<'a>(index: &'a Index, address: &str) -> Result<&'a NodeRec, Error> {
    let ids = resolve_all(index, address)?;
    match ids.as_slice() {
        [id] => Ok(index.get(*id)),
        [] => Err(Error::NotFound(address.to_string())),
        _ => Err(Error::Ambiguous(address.to_string())),
    }
}

fn collect_descendants(id: Uuid, index: &Index, out: &mut Vec<Uuid>) {
    let mut seen = HashSet::new();
    seen.insert(id);
    collect_descendants_seen(id, index, out, &mut seen);
}

fn collect_descendants_seen(
    id: Uuid,
    index: &Index,
    out: &mut Vec<Uuid>,
    seen: &mut HashSet<Uuid>,
) {
    let Some(children) = index.children.get(&id) else {
        return;
    };
    for child in children {
        if seen.insert(*child) {
            out.push(*child);
            collect_descendants_seen(*child, index, out, seen);
        }
    }
}

/// A Program's or Project's path. A Project whose Program is missing is an
/// inconsistent catalog, reported as an error rather than a panic.
fn path_of(node: &NodeRec, index: &Index) -> Result<Option<String>, Error> {
    Ok(match node.node_type {
        NodeType::Program => node.name.as_ref().map(|name| format!("/{name}")),
        NodeType::Project => {
            let program = node
                .parent_id
                .and_then(|id| index.lookup(id))
                .ok_or_else(|| {
                    Error::Inconsistent(format!("project {} has no parent program", node.id))
                })?;
            match (&program.name, &node.name) {
                (Some(program), Some(project)) => Some(format!("/{program}/{project}")),
                _ => None,
            }
        }
        _ => None,
    })
}

fn map_write(err: rusqlite::Error) -> Error {
    if let rusqlite::Error::SqliteFailure(failure, _) = &err {
        match failure.extended_code {
            ffi::SQLITE_CONSTRAINT_UNIQUE | ffi::SQLITE_CONSTRAINT_PRIMARYKEY => {
                return Error::SiblingNameTaken;
            }
            ffi::SQLITE_CONSTRAINT_FOREIGNKEY => return Error::IllegalParent,
            _ => {}
        }
    }
    Error::Sqlite(err)
}

fn run_statement(conn: &Connection, sql: &str) -> Result<SqlOutcome, Error> {
    let mut stmt = conn.prepare(sql)?;
    let count = stmt.column_count();
    if count == 0 {
        let rows_affected = stmt.execute([])?;
        return Ok(SqlOutcome::Executed { rows_affected });
    }
    let columns = stmt.column_names().into_iter().map(String::from).collect();
    let mut query = stmt.query([])?;
    let mut rows = Vec::new();
    while let Some(row) = query.next()? {
        let mut cells = Vec::with_capacity(count);
        for index in 0..count {
            cells.push(format_value(row.get_ref(index)?));
        }
        rows.push(cells);
    }
    Ok(SqlOutcome::Rows { columns, rows })
}

fn format_value(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => String::new(),
        ValueRef::Integer(value) => value.to_string(),
        ValueRef::Real(value) => value.to_string(),
        ValueRef::Text(value) | ValueRef::Blob(value) => {
            String::from_utf8_lossy(value).into_owned()
        }
    }
}
