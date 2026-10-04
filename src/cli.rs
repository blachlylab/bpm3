//! Command line for the catalog notebook. Flag spelling for `bpm query` follows
//! the PRD §4.8 sketch and may change.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

use crate::catalog::{self, Catalog, EntityQuery, SqlOutcome};
use crate::error::Error;
use crate::model::NodeType;
use crate::query::{self, RenderFormat};

#[derive(Parser)]
#[command(
    name = "bpm",
    about = "Biodata Project Manager",
    arg_required_else_help = true
)]
struct Cli {
    /// Catalog file. Overrides BPM_CATALOG. Defaults to ~/.bpm/default.db.
    #[arg(long, global = true)]
    catalog: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a catalog.
    Init {
        /// File to create. Defaults to ~/.bpm/default.db, or to --catalog when that flag is set.
        path: Option<PathBuf>,
        /// Replace an existing catalog file.
        #[arg(long)]
        force: bool,
    },
    /// Print the resolved catalog path.
    Catalog,
    /// Create an entity.
    Create {
        #[arg(value_enum)]
        kind: Kind,
        /// Program or project name.
        #[arg(long)]
        name: Option<String>,
        /// Parent path or UUID. Not allowed on a program.
        #[arg(long)]
        parent: Option<String>,
    },
    /// Rename a program or project.
    Rename { target: String, new_name: String },
    /// Move an entity under a new legal parent.
    Reparent { target: String, new_parent: String },
    /// Delete an entity. Refuses when children or linked files remain.
    Delete {
        target: String,
        /// Delete descendant entities, their metadata, and their file links.
        #[arg(long)]
        cascade: bool,
    },
    /// Set, read, or remove metadata.
    Meta {
        #[command(subcommand)]
        action: MetaCmd,
    },
    /// Run one SQL statement. Read-only unless --write.
    Sql {
        /// Open the catalog for writing. This does not record command history.
        #[arg(long)]
        write: bool,
        #[arg(allow_hyphen_values = true)]
        statement: String,
    },
    /// Structured query. The flag spelling is the §4.8 sketch.
    Query {
        #[command(subcommand)]
        what: QueryCmd,
    },
    /// Govern client. No server is configured in Core.
    Login,
    /// Govern client. No server is configured in Core.
    Logout,
    /// Govern client. No server is configured in Core.
    Use {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        _args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum MetaCmd {
    Set {
        target: String,
        key: String,
        #[arg(allow_hyphen_values = true)]
        value: String,
    },
    Get {
        target: String,
        key: Option<String>,
    },
    Unset {
        target: String,
        key: String,
    },
}

#[derive(Subcommand)]
enum QueryCmd {
    Entities {
        /// Program path, project path, UUID, or metadata path. Includes that entity and its descendants.
        #[arg(long)]
        under: Option<String>,
        /// program, project, case, sample, raw_data, or analysis.
        #[arg(long = "type", value_enum)]
        kind: Option<Kind>,
        /// key:value, key:, or :value. Repeatable. All must match.
        #[arg(long = "where")]
        wheres: Vec<String>,
        #[arg(long, value_enum, default_value = "table")]
        format: OutFmt,
    },
    Files {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        _args: Vec<String>,
    },
    Impact {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        _args: Vec<String>,
    },
    Lineage {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        _args: Vec<String>,
    },
    Summary,
}

#[derive(Clone, Copy, ValueEnum)]
#[value(rename_all = "snake_case")]
enum Kind {
    Program,
    Project,
    Case,
    Sample,
    RawData,
    Analysis,
}

impl From<Kind> for NodeType {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::Program => Self::Program,
            Kind::Project => Self::Project,
            Kind::Case => Self::Case,
            Kind::Sample => Self::Sample,
            Kind::RawData => Self::RawData,
            Kind::Analysis => Self::Analysis,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
#[value(rename_all = "lowercase")]
enum OutFmt {
    Table,
    Csv,
    Json,
}

impl From<OutFmt> for RenderFormat {
    fn from(format: OutFmt) -> Self {
        match format {
            OutFmt::Table => Self::Table,
            OutFmt::Csv => Self::Csv,
            OutFmt::Json => Self::Json,
        }
    }
}

pub fn run() -> ExitCode {
    match dispatch() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("bpm: {err}");
            ExitCode::from(1)
        }
    }
}

fn dispatch() -> Result<(), Error> {
    crate::migrate::check_contiguous()?;
    let cli = Cli::parse();
    match cli.command {
        Command::Init { path, force } => {
            let path = match path {
                Some(path) => path,
                None => match &cli.catalog {
                    Some(path) => path.clone(),
                    None => catalog::default_catalog_path()?,
                },
            };
            Catalog::init(&path, force)?;
            println!("{}", path.display());
        }
        Command::Catalog => {
            let path = catalog::resolve_catalog_path(cli.catalog.as_deref())?;
            if !path.is_file() {
                return Err(Error::MissingCatalog { path });
            }
            println!("{}", path.display());
        }
        Command::Create { kind, name, parent } => {
            let mut catalog = open_write(cli.catalog.as_deref())?;
            let printed = catalog.create(kind.into(), name.as_deref(), parent.as_deref())?;
            println!("{printed}");
        }
        Command::Rename { target, new_name } => {
            let mut catalog = open_write(cli.catalog.as_deref())?;
            catalog.rename(&target, &new_name)?;
        }
        Command::Reparent { target, new_parent } => {
            let mut catalog = open_write(cli.catalog.as_deref())?;
            catalog.reparent(&target, &new_parent)?;
        }
        Command::Delete { target, cascade } => {
            let mut catalog = open_write(cli.catalog.as_deref())?;
            catalog.delete(&target, cascade)?;
        }
        Command::Meta { action } => match action {
            MetaCmd::Set { target, key, value } => {
                let mut catalog = open_write(cli.catalog.as_deref())?;
                catalog.meta_set(&target, &key, &value)?;
            }
            MetaCmd::Get { target, key } => {
                let mut catalog = open_read(cli.catalog.as_deref())?;
                let pairs = catalog.meta_get(&target, key.as_deref())?;
                if key.is_some() {
                    println!("{}", pairs[0].1);
                } else {
                    for (meta_key, meta_value) in pairs {
                        println!("{meta_key}\t{meta_value}");
                    }
                }
            }
            MetaCmd::Unset { target, key } => {
                let mut catalog = open_write(cli.catalog.as_deref())?;
                catalog.meta_unset(&target, &key)?;
            }
        },
        Command::Sql { write, statement } => {
            let mut catalog = if write {
                open_write(cli.catalog.as_deref())?
            } else {
                open_read(cli.catalog.as_deref())?
            };
            match catalog.run_sql(&statement)? {
                SqlOutcome::Rows { columns, rows } => {
                    print!("{}", query::render_sql(&columns, &rows))
                }
                SqlOutcome::Executed { rows_affected } => {
                    println!("rows affected: {rows_affected}")
                }
            }
        }
        Command::Query { what } => match what {
            QueryCmd::Entities {
                under,
                kind,
                wheres,
                format,
            } => {
                let mut parsed = Vec::with_capacity(wheres.len());
                for selector in &wheres {
                    parsed.push(crate::model::parse_selector(selector)?);
                }
                let mut catalog = open_read(cli.catalog.as_deref())?;
                let rows = catalog.query_entities(&EntityQuery {
                    under,
                    node_type: kind.map(NodeType::from),
                    wheres: parsed,
                })?;
                print!("{}", query::render_entities(&rows, format.into()));
            }
            QueryCmd::Files { .. } => return Err(Error::FileMilestone),
            QueryCmd::Impact { .. } => return Err(Error::NotThisMilestone("impact")),
            QueryCmd::Lineage { .. } => return Err(Error::NotThisMilestone("lineage")),
            QueryCmd::Summary => return Err(Error::NotThisMilestone("summary")),
        },
        Command::Login | Command::Logout | Command::Use { .. } => return Err(Error::NoServer),
    }
    Ok(())
}

fn open_read(flag: Option<&std::path::Path>) -> Result<Catalog, Error> {
    Catalog::open_read(&catalog::resolve_catalog_path(flag)?)
}

fn open_write(flag: Option<&std::path::Path>) -> Result<Catalog, Error> {
    Catalog::open_write(&catalog::resolve_catalog_path(flag)?)
}
