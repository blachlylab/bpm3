//! Forward-only catalog migrations. Each embedded file runs in one transaction,
//! and the last step of that transaction sets `catalog_meta.schema_version`.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior};

use crate::error::Error;

/// `PRAGMA application_id` of every BPM catalog: "BPMC" in ASCII. `bpm init`
/// writes it before the first migration, so a SQLite file without it is not a
/// catalog and is never migrated.
pub const APPLICATION_ID: i32 = 0x4250_4D43;

pub struct Migration {
    pub version: i64,
    pub sql: &'static str,
}

pub static MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: include_str!("../migrations/V001__init.sql"),
}];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaState {
    /// A BPM catalog with no tables yet.
    Empty,
    /// A SQLite file that `bpm init` did not create.
    Foreign,
    Version(i64),
    Corrupt,
}

pub fn latest() -> i64 {
    MIGRATIONS.last().map(|m| m.version).unwrap_or(0)
}

pub fn check_contiguous() -> Result<(), Error> {
    for (index, migration) in MIGRATIONS.iter().enumerate() {
        let expected = index as i64 + 1;
        if migration.version != expected {
            return Err(Error::Message(format!(
                "embedded migrations must be numbered 1, 2, 3, … with no gaps (found {} at position {})",
                migration.version, index
            )));
        }
    }
    if MIGRATIONS.is_empty() {
        return Err(Error::Message("no migrations are embedded".into()));
    }
    Ok(())
}

pub fn inspect(conn: &Connection) -> Result<SchemaState, Error> {
    let application_id: i32 = conn.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    if application_id != APPLICATION_ID {
        return Ok(SchemaState::Foreign);
    }
    let tables: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\'",
        [],
        |row| row.get(0),
    )?;
    if tables == 0 {
        return Ok(SchemaState::Empty);
    }
    // Every migration writes its tables and the version in one transaction, so
    // tables without a valid version mean something else changed this file.
    if !table_exists(conn, "catalog_meta")? {
        return Ok(SchemaState::Corrupt);
    }
    let version = conn
        .query_row(
            "SELECT value FROM catalog_meta WHERE key = 'schema_version'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .and_then(|value| value.parse::<i64>().ok());
    Ok(match version {
        Some(version) if version >= 1 => SchemaState::Version(version),
        _ => SchemaState::Corrupt,
    })
}

/// Refuse anything other than the newest embedded version. Does not write.
pub fn ensure_current(conn: &Connection) -> Result<(), Error> {
    check_contiguous()?;
    let supported = latest();
    match inspect(conn)? {
        SchemaState::Version(found) if found == supported => Ok(()),
        SchemaState::Version(found) if found > supported => {
            Err(Error::SchemaNewer { found, supported })
        }
        SchemaState::Version(found) => Err(Error::SchemaOlder { found, supported }),
        SchemaState::Empty => Err(Error::SchemaOlder {
            found: 0,
            supported,
        }),
        SchemaState::Corrupt => Err(Error::CorruptSchema),
        SchemaState::Foreign => Err(Error::NotACatalog(path_of(conn))),
    }
}

/// Apply every migration newer than the stored version. Each file is one
/// `BEGIN IMMEDIATE` transaction, so it holds the catalog write lock.
pub fn apply(conn: &mut Connection) -> Result<(), Error> {
    check_contiguous()?;
    let supported = latest();
    let current = match inspect(conn)? {
        SchemaState::Empty => 0,
        SchemaState::Version(found) if found > supported => {
            return Err(Error::SchemaNewer { found, supported });
        }
        SchemaState::Version(found) => found,
        SchemaState::Corrupt => return Err(Error::CorruptSchema),
        SchemaState::Foreign => return Err(Error::NotACatalog(path_of(conn))),
    };
    for migration in MIGRATIONS.iter().filter(|m| m.version > current) {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(migration.sql)
            .map_err(|err| Error::MigrationFailed {
                version: migration.version,
                message: sqlite_message(err),
            })?;
        tx.execute(
            "INSERT INTO catalog_meta (key, value) VALUES ('schema_version', ?)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            [migration.version.to_string()],
        )?;
        tx.commit()?;
    }
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool, Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name = ?",
        [name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// SQLite's own message, without the SQL text rusqlite attaches to a batch
/// error, which would print the whole migration file.
fn sqlite_message(err: rusqlite::Error) -> String {
    match err {
        rusqlite::Error::SqliteFailure(_, Some(message)) => message,
        rusqlite::Error::SqlInputError { msg, .. } => msg,
        other => other.to_string(),
    }
}

fn path_of(conn: &Connection) -> std::path::PathBuf {
    conn.path().unwrap_or_default().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_start_at_one_and_do_not_skip() {
        check_contiguous().unwrap();
        assert_eq!(latest(), 1);
    }
}
