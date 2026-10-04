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
    /// The file rebuilds a table that other tables reference. The runner turns
    /// `foreign_keys` off before `BEGIN`, runs `PRAGMA foreign_key_check` before
    /// `COMMIT`, and turns `foreign_keys` back on afterwards: SQLite's
    /// twelve-step procedure, described in the migrations doc.
    pub rebuilds: bool,
}

pub static MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: include_str!("../migrations/V001__init.sql"),
        rebuilds: false,
    },
    Migration {
        version: 2,
        sql: include_str!("../migrations/V002__file_indexes.sql"),
        rebuilds: false,
    },
];

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
    check_sequence(MIGRATIONS)
}

fn check_sequence(migrations: &[Migration]) -> Result<(), Error> {
    for (index, migration) in migrations.iter().enumerate() {
        let expected = index as i64 + 1;
        if migration.version != expected {
            return Err(Error::Message(format!(
                "embedded migrations must be numbered 1, 2, 3, … with no gaps (found {} at position {})",
                migration.version, index
            )));
        }
    }
    if migrations.is_empty() {
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
    apply_list(conn, MIGRATIONS)
}

fn apply_list(conn: &mut Connection, migrations: &[Migration]) -> Result<(), Error> {
    check_sequence(migrations)?;
    let supported = migrations.last().map(|m| m.version).unwrap_or(0);
    let current = stored_version(conn, supported)?;
    for migration in migrations.iter().filter(|m| m.version > current) {
        if migration.rebuilds {
            // The pragma is a no-op inside a transaction, so it is set first.
            conn.pragma_update(None, "foreign_keys", false)?;
        }
        let result = apply_one(conn, migration, supported);
        if migration.rebuilds {
            conn.pragma_update(None, "foreign_keys", true)?;
        }
        result?;
    }
    Ok(())
}

/// Run one file under the write lock. The version is read again inside the
/// transaction, so a migration another writer finished since the first read
/// is skipped instead of failing on tables that already exist.
fn apply_one(conn: &mut Connection, migration: &Migration, supported: i64) -> Result<(), Error> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if stored_version(&tx, supported)? >= migration.version {
        return Ok(());
    }
    tx.execute_batch(migration.sql)
        .map_err(|err| Error::MigrationFailed {
            version: migration.version,
            message: sqlite_message(err),
        })?;
    if migration.rebuilds {
        let broken: i64 =
            tx.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        if broken > 0 {
            return Err(Error::MigrationFailed {
                version: migration.version,
                message: format!("{broken} row(s) break a foreign key after the rebuild"),
            });
        }
    }
    tx.execute(
        "INSERT INTO catalog_meta (key, value) VALUES ('schema_version', ?)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [migration.version.to_string()],
    )?;
    tx.commit()?;
    Ok(())
}

/// The stored version, 0 for an empty catalog. Anything this runner must not
/// touch is an error.
fn stored_version(conn: &Connection, supported: i64) -> Result<i64, Error> {
    match inspect(conn)? {
        SchemaState::Empty => Ok(0),
        SchemaState::Version(found) if found > supported => {
            Err(Error::SchemaNewer { found, supported })
        }
        SchemaState::Version(found) => Ok(found),
        SchemaState::Corrupt => Err(Error::CorruptSchema),
        SchemaState::Foreign => Err(Error::NotACatalog(path_of(conn))),
    }
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
        assert_eq!(latest(), 2);
    }

    const BASE: Migration = Migration {
        version: 1,
        sql: "CREATE TABLE catalog_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
              CREATE TABLE parent (id TEXT PRIMARY KEY, label TEXT) STRICT;
              CREATE TABLE child (id TEXT PRIMARY KEY,
                  parent_id TEXT NOT NULL REFERENCES parent (id) ON DELETE RESTRICT) STRICT;
              INSERT INTO parent VALUES ('p1', 'one');
              INSERT INTO child VALUES ('c1', 'p1');",
        rebuilds: false,
    };

    /// Rebuild `parent` with a NOT NULL label: create, copy, drop, rename.
    const REBUILD_SQL: &str = "
        CREATE TABLE parent_new (id TEXT PRIMARY KEY, label TEXT NOT NULL) STRICT;
        INSERT INTO parent_new SELECT id, label FROM parent;
        DROP TABLE parent;
        ALTER TABLE parent_new RENAME TO parent;";

    fn fresh() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "application_id", APPLICATION_ID)
            .unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        conn
    }

    fn version(conn: &Connection) -> i64 {
        match inspect(conn).unwrap() {
            SchemaState::Version(found) => found,
            other => panic!("{other:?}"),
        }
    }

    fn foreign_keys(conn: &Connection) -> bool {
        conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn a_rebuild_of_a_referenced_table_runs_with_foreign_keys_off() {
        // Without the procedure, dropping the referenced table fails.
        let mut plain = fresh();
        let unflagged = [
            BASE,
            Migration {
                version: 2,
                sql: REBUILD_SQL,
                rebuilds: false,
            },
        ];
        assert!(matches!(
            apply_list(&mut plain, &unflagged),
            Err(Error::MigrationFailed { version: 2, .. })
        ));
        assert_eq!(version(&plain), 1);

        let mut conn = fresh();
        let flagged = [
            BASE,
            Migration {
                version: 2,
                sql: REBUILD_SQL,
                rebuilds: true,
            },
        ];
        apply_list(&mut conn, &flagged).unwrap();
        assert_eq!(version(&conn), 2);
        assert!(foreign_keys(&conn));
        let check: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(check, 0);
        // The child's key now points at the rebuilt table and is still enforced.
        assert!(
            conn.execute("INSERT INTO child VALUES ('c2', 'nope')", [])
                .is_err()
        );
    }

    #[test]
    fn a_rebuild_that_breaks_a_foreign_key_rolls_back() {
        let mut conn = fresh();
        let lossy = [
            BASE,
            Migration {
                version: 2,
                sql: "CREATE TABLE parent_new (id TEXT PRIMARY KEY, label TEXT) STRICT;
                      DROP TABLE parent;
                      ALTER TABLE parent_new RENAME TO parent;",
                rebuilds: true,
            },
        ];
        let err = apply_list(&mut conn, &lossy).unwrap_err();
        assert!(err.to_string().contains("break a foreign key"), "{err}");
        assert_eq!(version(&conn), 1);
        assert!(foreign_keys(&conn));
        let parents: i64 = conn
            .query_row("SELECT COUNT(*) FROM parent", [], |row| row.get(0))
            .unwrap();
        assert_eq!(parents, 1);
    }

    #[test]
    fn a_migration_another_writer_finished_is_skipped() {
        let mut conn = fresh();
        let list = [
            BASE,
            Migration {
                version: 2,
                sql: "CREATE TABLE extra (id TEXT PRIMARY KEY) STRICT;",
                rebuilds: false,
            },
        ];
        apply_list(&mut conn, &list).unwrap();
        // A writer that read version 1 before taking the lock would run V002
        // again. Inside the transaction it sees version 2 and does nothing.
        apply_one(&mut conn, &list[1], 2).unwrap();
        assert_eq!(version(&conn), 2);
    }
}
