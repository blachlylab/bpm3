use std::path::PathBuf;

use crate::model::ModelError;

/// Errors the CLI and a later UI can match on without parsing prose beyond [`Display`].
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("illegal parent")]
    IllegalParent,

    #[error("sibling name taken")]
    SiblingNameTaken,

    #[error("a name is required")]
    NameRequired,

    #[error("a parent is required")]
    ParentRequired,

    #[error("{0} does not take a name")]
    NameNotAllowed(&'static str),

    #[error("only a program or a project can be renamed")]
    NotRenameable,

    #[error("a program has no parent")]
    ProgramNoParent,

    #[error("entity has children or linked files")]
    HasDependents,

    #[error("no entity matches {0}")]
    NotFound(String),

    #[error("address matches more than one entity: {0}")]
    Ambiguous(String),

    #[error("metadata key {0} is not set")]
    MetaMissing(String),

    #[error("{path} does not exist; run `bpm init`")]
    MissingCatalog { path: PathBuf },

    #[error("catalog already exists: {0}")]
    AlreadyExists(PathBuf),

    #[error("catalog is busy: {0}")]
    Busy(PathBuf),

    #[error("catalog schema version {found} is newer than this bpm binary (version {supported})")]
    SchemaNewer { found: i64, supported: i64 },

    #[error(
        "catalog schema version {found} is older than this bpm binary (version {supported}); run a writing command to migrate"
    )]
    SchemaOlder { found: i64, supported: i64 },

    #[error("{0} is not a BPM catalog; refusing to open it")]
    NotACatalog(PathBuf),

    #[error("catalog has no valid schema_version; refusing to open it")]
    CorruptSchema,

    #[error("migration V{version:03} failed: {message}")]
    MigrationFailed { version: i64, message: String },

    #[error("catalog is inconsistent: {0}; run `bpm repair` to list the problems")]
    Inconsistent(String),

    #[error(
        "{0} row(s) break the catalog's integrity; see the repair guide, or `bpm repair --apply` removes them"
    )]
    RepairNeeded(usize),

    #[error("no file matches {0}")]
    FileNotFound(String),

    #[error("no location {0}")]
    LocationNotFound(String),

    #[error("file is not linked to {0}")]
    NotLinked(String),

    #[error("file has links; unlink them first, or pass --cascade")]
    FileLinked,

    #[error("acknowledge takes one file; {0} is a directory")]
    AcknowledgeDirectory(PathBuf),

    #[error("file {0} has no present location to read")]
    NoPresentLocation(String),

    #[error(
        "the present locations of file {id} do not hold the same bytes; restore or remove the wrong location, then acknowledge again:\n{listing}"
    )]
    LocationsDisagree { id: String, listing: String },

    #[error("{0} path(s) could not be read; they are listed above")]
    PathErrors(usize),

    #[error("{0} is not part of this milestone")]
    NotThisMilestone(&'static str),

    #[error("no server is configured")]
    NoServer,

    #[error(transparent)]
    Model(#[from] ModelError),

    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Message(String),
}
