//! Command line for the catalog notebook and the file indexer. Flag spelling
//! for `bpm query` follows the PRD §4.8 sketch and may change.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

use crate::catalog::{self, AckOutcome, Catalog, EntityQuery, FileQuery, LinkOptions, SqlOutcome};
use crate::error::Error;
use crate::ingest::filter::{self, FilterSpec, PathFilter};
use crate::ingest::{self, AckReport, ScanArg};
use crate::link::{Expect, MatchOn, Renderer, Table, Template};
use crate::model::{Drift, FileRef, NodeType, POSIX};
use crate::perms;
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
    /// Delete an entity, a location, or a file row. Never deletes bytes.
    /// Refuses an entity with children or linked files, and a linked file.
    Delete {
        /// Entity path or UUID.
        #[arg(required_unless_present_any = ["location", "file"], conflicts_with_all = ["location", "file"])]
        target: Option<String>,
        /// Remove this location from its file. The file and its other locations stay.
        #[arg(long, conflicts_with = "file")]
        location: Option<String>,
        /// Remove this file row (id or location path) with its locations and digests.
        #[arg(long)]
        file: Option<String>,
        /// Also delete descendant entities, their metadata, and their file links;
        /// or, with --file, that file's links.
        #[arg(long)]
        cascade: bool,
    },
    /// Add the files under a directory that are not already in the catalog.
    Ingest {
        path: PathBuf,
        #[command(flatten)]
        filters: FilterArgs,
    },
    /// Check locations already in the catalog for drift and store digests.
    /// No argument scans every location; a backend or a path narrows that.
    Scan {
        /// A backend (posix, s3) or a path. Write ./posix for a directory named posix.
        target: Option<String>,
        /// Also compute and store MD5.
        #[arg(long)]
        md5: bool,
        #[command(flatten)]
        filters: FilterArgs,
    },
    /// Accept the bytes now at one file's locations as its next generation.
    /// Takes exactly one file; never a directory or a set of files.
    Acknowledge {
        /// File id, or the path of one of its locations.
        file: String,
        /// Also compute and store MD5 for the accepted bytes.
        #[arg(long)]
        md5: bool,
    },
    /// Link files to entities: one file, or every file a pattern or a table
    /// places. The guide is docs/guide/link.md.
    Link {
        /// File ids, or catalog locations. A location that is a directory
        /// reaches every location under it.
        #[arg(required = true, value_name = "FILE|LOCATION")]
        operands: Vec<String>,
        #[command(flatten)]
        select: LinkSelect,
        /// The role of every link. Placeholders work here too, such as {role}
        /// from a --table column.
        #[arg(long)]
        role: String,
        /// Allow roles that are not on this catalog's list, and add them to it.
        #[arg(long)]
        new_role: bool,
        /// Change the role of a link that already exists.
        #[arg(long)]
        set_role: bool,
        /// Link a file even when it is already linked to a node of the target
        /// type under the anchor and this run would create a node for it.
        #[arg(long)]
        relink: bool,
        /// Every TYPE node the run touches gets N (or N..M) files. Repeatable.
        #[arg(long, value_name = "TYPE=N[..M]")]
        expect: Vec<String>,
        /// Fail when a file does not match the pattern, or a table row matches no file.
        #[arg(long)]
        strict: bool,
        #[command(flatten)]
        show: ShowArgs,
        /// Apply a plan that creates entities without asking.
        #[arg(short, long)]
        yes: bool,
    },
    /// Detach files from entities. The file rows stay. Takes the same
    /// template as link, and never creates.
    Unlink {
        #[arg(required = true, value_name = "FILE|LOCATION")]
        operands: Vec<String>,
        #[command(flatten)]
        select: LinkSelect,
        #[command(flatten)]
        show: ShowArgs,
    },
    /// Undo one link run: remove its links and the entities it created, and
    /// restore roles it changed.
    Undo {
        /// The run id `bpm link` printed.
        #[arg(required_unless_present = "list", conflicts_with = "list")]
        run: Option<String>,
        /// List link runs, newest first.
        #[arg(long)]
        list: bool,
        #[command(flatten)]
        show: ShowArgs,
        /// Remove entities without asking.
        #[arg(short, long)]
        yes: bool,
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
    /// Check the catalog's integrity. With --apply, remove the rows that break it.
    Repair {
        /// Remove every reported row in one transaction. An entity whose parent
        /// is missing is removed with its descendants, metadata, and file links.
        #[arg(long)]
        apply: bool,
    },
    /// Structured query. The flag spelling is the §4.8 sketch.
    Query {
        #[command(subcommand)]
        what: QueryCmd,
    },
    /// Serve the read-only web UI. Listens on 127.0.0.1:3000 unless told otherwise.
    Serve {
        /// Address to listen on. Setting it, to any value, requires a token.
        #[arg(long, env = "BPM_HOST")]
        host: Option<String>,
        /// Port to listen on. 0 picks a free port and prints it.
        #[arg(long, env = "BPM_PORT")]
        port: Option<u16>,
        /// Token the login page asks for. Never printed.
        #[arg(long, env = "BPM_TOKEN", hide_env_values = true)]
        token: Option<String>,
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

#[derive(clap::Args)]
struct FilterArgs {
    /// Comma-separated globs that replace the blacklist in ~/.bpm/config.toml for this run.
    #[arg(long, value_name = "PATTERNS")]
    blacklist: Vec<String>,
    /// Do not skip the built-in names (.DS_Store, Thumbs.db).
    #[arg(long)]
    no_default_blacklist: bool,
    /// Comma-separated globs that replace the whitelist in ~/.bpm/config.toml for this run.
    /// When the list in effect is non-empty, only matching paths are considered.
    #[arg(long, value_name = "PATTERNS")]
    whitelist: Vec<String>,
}

impl FilterArgs {
    fn build(&self) -> Result<PathFilter, Error> {
        let patterns = |values: &[String]| {
            (!values.is_empty()).then(|| {
                values
                    .iter()
                    .flat_map(|raw| filter::split_patterns(raw))
                    .collect()
            })
        };
        let spec = FilterSpec {
            blacklist: patterns(&self.blacklist),
            no_default_blacklist: self.no_default_blacklist,
            whitelist: patterns(&self.whitelist),
        };
        // The file is read when a run still takes at least one list from it.
        // With no HOME there is no config file, which is two empty lists.
        let global = if spec.blacklist.is_none() || spec.whitelist.is_none() {
            match catalog::home_dir() {
                Ok(home) => filter::global_lists(&home)?,
                Err(_) => filter::GlobalLists::default(),
            }
        } else {
            filter::GlobalLists::default()
        };
        PathFilter::new(&spec, global)
    }
}

#[derive(clap::Args)]
struct LinkSelect {
    /// Where each file links: an address, then typed steps such as
    /// case[subject_id:{1}]?/sample?/raw_data+.
    #[arg(long, value_name = "TEMPLATE")]
    to: String,
    /// A regex that must match a file's whole name. Its groups are {1}, {2},
    /// and {name}.
    #[arg(long = "match", value_name = "REGEX", conflicts_with = "match_path")]
    pattern: Option<String>,
    /// Like --match, against the path relative to the location operand.
    #[arg(long, value_name = "REGEX")]
    match_path: Option<String>,
    /// A .tsv or .csv mapping table. Its columns become placeholders.
    #[arg(long, value_name = "FILE", requires = "join")]
    table: Option<PathBuf>,
    /// COLUMN=TEMPLATE: each file uses the one row whose COLUMN equals TEMPLATE.
    #[arg(long, value_name = "COLUMN=TEMPLATE", requires = "table")]
    join: Option<String>,
}

#[derive(clap::Args)]
struct ShowArgs {
    /// Print the plan and stop.
    #[arg(short = 'n', long)]
    dry_run: bool,
    /// Also print one line per file: action, location, target, role.
    #[arg(long)]
    detail: bool,
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
        /// Program path, project path, UUID, or metadata path. Files linked to
        /// that entity or any descendant.
        #[arg(long)]
        under: Option<String>,
        /// Only links with this role.
        #[arg(long)]
        role: Option<String>,
        /// Files with no link.
        #[arg(long)]
        unlinked: bool,
        /// Files with a location in this state. Repeatable.
        #[arg(long, value_enum)]
        drift: Vec<DriftArg>,
        /// algorithm:hex, such as blake3:<hex>. Matches a current digest.
        #[arg(long)]
        digest: Option<String>,
        /// Print how many files match, instead of the rows.
        #[arg(short, long)]
        count: bool,
        #[arg(long, value_enum, default_value = "table")]
        format: OutFmt,
    },
    Impact {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        _args: Vec<String>,
    },
    Lineage {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        _args: Vec<String>,
    },
    Summary {
        #[arg(long, value_enum, default_value = "table")]
        format: OutFmt,
    },
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
#[value(rename_all = "snake_case")]
enum DriftArg {
    Missing,
    StatChanged,
    DigestMismatch,
    Ok,
    Unverified,
}

impl From<DriftArg> for Drift {
    fn from(arg: DriftArg) -> Self {
        match arg {
            DriftArg::Missing => Self::Missing,
            DriftArg::StatChanged => Self::StatChanged,
            DriftArg::DigestMismatch => Self::DigestMismatch,
            DriftArg::Ok => Self::Ok,
            DriftArg::Unverified => Self::Unverified,
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
    warn_if_bpm_dir_is_shared();
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
            let mut catalog = open_write_unchecked(cli.catalog.as_deref())?;
            catalog.reparent(&target, &new_parent)?;
        }
        Command::Delete {
            target,
            location,
            file,
            cascade,
        } => {
            let mut catalog = open_write_unchecked(cli.catalog.as_deref())?;
            if let Some(location) = location {
                catalog.delete_location(POSIX, &ingest::location_uri(&location)?)?;
            } else if let Some(file) = file {
                catalog.delete_file(&ingest::file_ref(&file)?, cascade)?;
            } else if let Some(target) = target {
                catalog.delete(&target, cascade)?;
            }
        }
        Command::Ingest { path, filters } => {
            let filter = filters.build()?;
            let mut catalog = open_write_unchecked(cli.catalog.as_deref())?;
            let report = ingest::ingest(&mut catalog, &path, &filter)?;
            if report.broken_links > 0 {
                let noun = if report.broken_links == 1 {
                    "broken symbolic link"
                } else {
                    "broken symbolic links"
                };
                eprintln!("bpm: {} {noun}", report.broken_links);
            }
            for (uri, message) in &report.errors {
                eprintln!("bpm: {uri}: {message}");
            }
            for (uri, of) in &report.possible_duplicates {
                println!(
                    "possible duplicate: {uri} has the size and fingerprint of {}, which could not be hashed; not merged",
                    of.join(", ")
                );
            }
            println!(
                "ingest {}: {} files seen, {} new files, {} new locations, {} copies, {} already recorded, {} errors",
                report.run_id.map(|id| id.to_string()).unwrap_or_default(),
                report.seen,
                report.created,
                report.located,
                report.copies,
                report.already,
                report.errors.len()
            );
            if !report.errors.is_empty() {
                return Err(Error::PathErrors(report.errors.len()));
            }
        }
        Command::Scan {
            target,
            md5,
            filters,
        } => {
            let filter = filters.build()?;
            let mut catalog = open_write_unchecked(cli.catalog.as_deref())?;
            let report = ingest::scan(
                &mut catalog,
                &ScanArg::parse(target.as_deref()),
                &filter,
                md5,
            )?;
            for (uri, message) in &report.errors {
                eprintln!("bpm: {uri}: {message}");
            }
            for (state, uri) in &report.drifted {
                println!("{}\t{uri}", state.slug());
            }
            let counts = [
                Drift::Ok,
                Drift::Unverified,
                Drift::Missing,
                Drift::StatChanged,
                Drift::DigestMismatch,
            ]
            .iter()
            .map(|state| {
                format!(
                    "{} {}",
                    report.counts.get(state).copied().unwrap_or(0),
                    state.slug()
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
            println!(
                "scan {}: {} locations, {counts}, {} errors",
                report.run_id.map(|id| id.to_string()).unwrap_or_default(),
                report.seen,
                report.errors.len()
            );
            if !report.errors.is_empty() {
                return Err(Error::PathErrors(report.errors.len()));
            }
        }
        Command::Acknowledge { file, md5 } => {
            let file = ingest::file_ref(&file)?;
            let mut catalog = open_write_unchecked(cli.catalog.as_deref())?;
            match ingest::acknowledge(&mut catalog, &file, md5)? {
                AckReport::NothingToDo(row) => {
                    println!("file {}: no drift; nothing to acknowledge", row.id);
                }
                AckReport::Done { file, outcome } => {
                    let digest = file
                        .digests
                        .iter()
                        .find(|digest| digest.algorithm == "blake3")
                        .map(|digest| digest.wire())
                        .unwrap_or_default();
                    match outcome {
                        AckOutcome::NewGeneration(generation) => println!(
                            "file {}: generation {generation} is current ({digest})",
                            file.id
                        ),
                        AckOutcome::SameBytes(generation) => println!(
                            "file {}: bytes match generation {generation} ({digest}); stat accepted",
                            file.id
                        ),
                    }
                }
            }
        }
        Command::Link {
            operands,
            select,
            role,
            new_role,
            set_role,
            relink,
            expect,
            strict,
            show,
            yes,
        } => {
            let template = Template::parse(&select.to)?;
            let mut expects = Vec::with_capacity(expect.len());
            for raw in &expect {
                let parsed = Expect::parse(raw)?;
                if !template
                    .steps()
                    .iter()
                    .any(|(node_type, _)| *node_type == parsed.node_type)
                {
                    return Err(Error::Message(format!(
                        "--expect {raw}: the template has no {} step",
                        parsed.node_type.slug()
                    )));
                }
                expects.push(parsed);
            }
            let options = LinkOptions {
                expects,
                new_role,
                set_role,
                relink,
                unlink: false,
            };
            run_links(
                cli.catalog.as_deref(),
                template,
                &operands,
                &select,
                Some(&role),
                &options,
                strict,
                &show,
                yes,
            )?;
        }
        Command::Unlink {
            operands,
            select,
            show,
        } => {
            let template = Template::parse(&select.to)?;
            template.require_existing()?;
            let options = LinkOptions {
                unlink: true,
                ..LinkOptions::default()
            };
            run_links(
                cli.catalog.as_deref(),
                template,
                &operands,
                &select,
                None,
                &options,
                false,
                &show,
                true,
            )?;
        }
        Command::Undo {
            run,
            list,
            show,
            yes,
        } => {
            let mut catalog = open_write(cli.catalog.as_deref())?;
            if list {
                let rows: Vec<Vec<String>> = catalog
                    .link_runs()?
                    .into_iter()
                    .map(|row| {
                        vec![
                            row.id,
                            row.applied_at,
                            row.entities_created.to_string(),
                            row.links_created.to_string(),
                            row.roles_changed.to_string(),
                            row.undone_at.unwrap_or_default(),
                            command_line(&row.command),
                        ]
                    })
                    .collect();
                print!(
                    "{}",
                    query::render_sql(
                        &[
                            "run",
                            "applied_at",
                            "entities",
                            "links",
                            "roles",
                            "undone_at",
                            "command"
                        ]
                        .map(String::from),
                        &rows
                    )
                );
                return Ok(());
            }
            let run = run.unwrap_or_default();
            let plan = catalog.plan_undo(&run)?;
            if !plan.blockers.is_empty() {
                return Err(problems(&plan.blockers, "undo"));
            }
            let mut by_type: Vec<(NodeType, usize)> = Vec::new();
            for (node_type, _) in &plan.entities {
                match by_type.iter_mut().find(|(t, _)| t == node_type) {
                    Some(entry) => entry.1 += 1,
                    None => by_type.push((*node_type, 1)),
                }
            }
            println!(
                "undo {} (applied {}): remove {} links and {} entities{}, restore {} roles",
                plan.run.id,
                plan.run.applied_at,
                plan.links,
                plan.entities.len(),
                describe_counts(&by_type),
                plan.roles.len()
            );
            if show.detail {
                for (node_type, id) in &plan.entities {
                    println!("delete\t{}\t{id}", node_type.slug());
                }
                for (file_id, node_type, node_id, now, restore) in &plan.roles {
                    println!(
                        "restore\t{file_id}\t{} {node_id}\t{now} -> {restore}",
                        node_type.slug()
                    );
                }
            }
            if show.dry_run {
                return Ok(());
            }
            if !plan.entities.is_empty() && !yes {
                confirm(&format!(
                    "this undo deletes {} entities",
                    plan.entities.len()
                ))?;
            }
            catalog.apply_undo(&run, &plan)?;
            println!("undone {}", plan.run.id);
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
                open_write_unchecked(cli.catalog.as_deref())?
            } else {
                open_read_unchecked(cli.catalog.as_deref())?
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
        Command::Repair { apply } => {
            if apply {
                let mut catalog = open_write_unchecked(cli.catalog.as_deref())?;
                let (removed, descendants) = catalog.repair()?;
                if removed.is_empty() {
                    println!("catalog is consistent");
                } else {
                    println!("removed:");
                    print_problems(&removed);
                    if descendants > 0 {
                        println!(
                            "{descendants} descendant entities of those, with their metadata and file links"
                        );
                    }
                }
            } else {
                let mut catalog = open_read_unchecked(cli.catalog.as_deref())?;
                let found = catalog.problems()?;
                if found.is_empty() {
                    println!("catalog is consistent");
                } else {
                    print_problems(&found);
                    let rows = found.iter().map(|problem| problem.keys.len()).sum();
                    return Err(Error::RepairNeeded(rows));
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
            QueryCmd::Files {
                under,
                role,
                unlinked,
                drift,
                digest,
                count,
                format,
            } => {
                let digest = digest
                    .as_deref()
                    .map(crate::model::parse_digest)
                    .transpose()?;
                let query = FileQuery {
                    under,
                    role,
                    unlinked,
                    drift: drift.into_iter().map(Drift::from).collect(),
                    digest,
                };
                let mut catalog = open_read(cli.catalog.as_deref())?;
                if count {
                    print!(
                        "{}",
                        query::render_count(catalog.count_files(&query)?, format.into())
                    );
                } else {
                    let rows = catalog.query_files(&query)?;
                    print!("{}", query::render_files(&rows, format.into()));
                }
            }
            QueryCmd::Impact { .. } => return Err(Error::NotThisMilestone("impact")),
            QueryCmd::Lineage { .. } => return Err(Error::NotThisMilestone("lineage")),
            QueryCmd::Summary { format } => {
                let mut catalog = open_read(cli.catalog.as_deref())?;
                print!(
                    "{}",
                    query::render_summary(&catalog.summary()?, format.into())
                );
            }
        },
        Command::Serve { host, port, token } => {
            let config = crate::web::ServeConfig::resolve(host, port, token)?;
            let path = catalog::resolve_catalog_path(cli.catalog.as_deref())?;
            crate::web::serve(path, config)?;
        }
        Command::Login | Command::Logout | Command::Use { .. } => return Err(Error::NoServer),
    }
    Ok(())
}

/// Plan a link or unlink run, show it, confirm it when it creates entities,
/// and apply it.
#[allow(clippy::too_many_arguments)]
fn run_links(
    flag: Option<&Path>,
    template: Template,
    operands: &[String],
    select: &LinkSelect,
    role: Option<&str>,
    options: &LinkOptions,
    strict: bool,
    show: &ShowArgs,
    yes: bool,
) -> Result<(), Error> {
    let pattern = match (&select.pattern, &select.match_path) {
        (Some(raw), _) => Some((raw.as_str(), MatchOn::Name)),
        (None, Some(raw)) => Some((raw.as_str(), MatchOn::Path)),
        (None, None) => None,
    };
    let table = match (&select.table, &select.join) {
        (Some(path), Some(join)) => Some(Table::load(path, join)?),
        _ => None,
    };
    let renderer = Renderer::new(template, role, pattern, table)?;
    let mut refs = Vec::with_capacity(operands.len());
    for operand in operands {
        refs.push(link_operand(operand)?);
    }
    // A writer, because link and unlink write; it migrates an older catalog.
    // Planning still reads a snapshot and does not hold the write lock.
    let mut catalog = open_write(flag)?;
    let sources = catalog.link_sources(&refs)?;
    let rendering = renderer.render(&sources);
    let plan = catalog.plan_links(&rendering.links, options)?;
    let verb = if options.unlink { "unlink" } else { "link" };

    let mut errors = rendering.errors.clone();
    if strict && !options.unlink {
        errors.extend(
            rendering
                .unmatched
                .iter()
                .map(|uri| format!("{uri} does not match the pattern (--strict)")),
        );
        errors.extend(
            rendering
                .unused_rows
                .iter()
                .map(|key| format!("table row {key:?} matches no file (--strict)")),
        );
    }
    errors.extend(plan.errors.iter().cloned());

    let matched = rendering.links.len();
    println!(
        "{} files reached, {matched} matched, {} not matched",
        sources
            .iter()
            .map(|source| &source.file_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        rendering.unmatched.len()
    );
    list("not matched by the pattern", &rendering.unmatched);
    if rendering.table_rows > 0 {
        list(
            &format!("of {} table rows, matched no file", rendering.table_rows),
            &rendering.unused_rows,
        );
    }
    if options.unlink {
        println!("unlink {}", plan.count("unlink"));
    } else {
        println!(
            "link {}, already linked {}, role changes {}",
            plan.count("link"),
            plan.count("already"),
            plan.count("set-role"),
        );
        if plan.count("skip") > 0 {
            println!(
                "skip {}: already linked to a {} under the anchor (--relink links them anyway)",
                plan.count("skip"),
                renderer
                    .template()
                    .steps()
                    .last()
                    .map(|(node_type, _)| node_type.slug())
                    .unwrap_or("node")
            );
        }
        if plan.created() > 0 {
            println!(
                "create {}",
                plan.creates()
                    .iter()
                    .map(|(node_type, count)| format!("{count} {}", node_type.slug()))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if !plan.new_roles.is_empty() {
            println!("new roles: {}", plan.new_roles.join(", "));
        }
        let mut roles: Vec<(String, usize)> = Vec::new();
        for link in plan
            .links
            .iter()
            .filter(|link| link.action.slug() == "link")
        {
            match roles.iter_mut().find(|(role, _)| *role == link.role) {
                Some(entry) => entry.1 += 1,
                None => roles.push((link.role.clone(), 1)),
            }
        }
        if !roles.is_empty() {
            println!(
                "roles: {}",
                roles
                    .iter()
                    .map(|(role, count)| format!("{role} {count}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    for note in &plan.notes {
        println!("note: {note}");
    }
    if show.detail {
        for link in &plan.links {
            println!(
                "{}\t{}\t{}\t{}",
                link.action.slug(),
                link.uri,
                link.target,
                link.role
            );
        }
    }
    if !errors.is_empty() {
        return Err(problems(&errors, verb));
    }
    if matched == 0 {
        return Err(Error::Message(format!("no files to {verb}")));
    }
    if show.dry_run {
        return Ok(());
    }
    if plan.created() > 0 && !yes {
        confirm(&format!("this run creates {} entities", plan.created()))?;
    }
    let command = serde_json::to_string(&std::env::args().collect::<Vec<_>>())
        .unwrap_or_else(|_| "[]".into());
    match catalog.apply_links(&rendering.links, options, &plan, &command)? {
        Some(run) => println!(
            "link run {run}: {} links, {} entities created, {} roles changed",
            plan.count("link"),
            plan.created(),
            plan.count("set-role")
        ),
        None if options.unlink => println!("unlinked {}", plan.count("unlink")),
        None => println!("nothing to write"),
    }
    Ok(())
}

/// A file id, a local path, or later an object-store URI.
fn link_operand(raw: &str) -> Result<FileRef, Error> {
    if let Some((scheme, _)) = raw.split_once("://") {
        return match scheme {
            "s3" => Err(Error::NotThisMilestone("an s3 location")),
            _ => Err(Error::Message(format!(
                "{raw}: unknown location scheme {scheme}"
            ))),
        };
    }
    ingest::file_ref(raw)
}

/// Ask on a terminal. Without one, a run that needs an answer fails.
fn confirm(what: &str) -> Result<(), Error> {
    use std::io::{BufRead, IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        return Err(Error::Message(format!(
            "{what}; pass --yes to apply it without a terminal, or -n to only see the plan"
        )));
    }
    eprint!("bpm: {what}. Apply? [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().lock().read_line(&mut answer)?;
    if matches!(answer.trim(), "y" | "Y" | "yes") {
        Ok(())
    } else {
        Err(Error::Message("not applied".into()))
    }
}

fn problems(messages: &[String], verb: &str) -> Error {
    for message in messages.iter().take(LISTED) {
        eprintln!("bpm: {message}");
    }
    if messages.len() > LISTED {
        eprintln!("bpm: … and {} more", messages.len() - LISTED);
    }
    Error::Message(format!(
        "{} problem(s); nothing was written ({verb})",
        messages.len()
    ))
}

fn list(heading: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    println!("{} {heading}:", items.len());
    for item in items.iter().take(LISTED) {
        println!("  {item}");
    }
    if items.len() > LISTED {
        println!("  … and {} more", items.len() - LISTED);
    }
}

/// " (500 case, 500 sample)", or "" when empty.
fn describe_counts(counts: &[(NodeType, usize)]) -> String {
    if counts.is_empty() {
        return String::new();
    }
    format!(
        " ({})",
        counts
            .iter()
            .map(|(node_type, count)| format!("{count} {}", node_type.slug()))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// The recorded argument vector, without the program name.
fn command_line(json: &str) -> String {
    serde_json::from_str::<Vec<String>>(json)
        .map(|args| args.into_iter().skip(1).collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|_| json.to_string())
}

/// Opens for reading and refuses a catalog whose entity tree is broken. Every
/// command that walks or extends the tree uses this or [`open_write`].
fn open_read(flag: Option<&Path>) -> Result<Catalog, Error> {
    let catalog = open_read_unchecked(flag)?;
    catalog.check_tree()?;
    Ok(catalog)
}

fn open_write(flag: Option<&Path>) -> Result<Catalog, Error> {
    let catalog = open_write_unchecked(flag)?;
    catalog.check_tree()?;
    Ok(catalog)
}

/// For the commands an operator uses to inspect or fix a broken tree:
/// `sql`, `repair`, `reparent`, and `delete`.
fn open_read_unchecked(flag: Option<&Path>) -> Result<Catalog, Error> {
    Catalog::open_read(&catalog::resolve_catalog_path(flag)?)
}

fn open_write_unchecked(flag: Option<&Path>) -> Result<Catalog, Error> {
    Catalog::open_write(&catalog::resolve_catalog_path(flag)?)
}

/// At most this many rows are listed per problem.
const LISTED: usize = 20;

fn print_problems(problems: &[catalog::Problem]) {
    for problem in problems {
        println!(
            "{}: {} row(s), {}",
            problem.table,
            problem.keys.len(),
            problem.issue
        );
        for key in problem.keys.iter().take(LISTED) {
            println!("  {key}");
        }
        if problem.keys.len() > LISTED {
            println!("  … and {} more", problem.keys.len() - LISTED);
        }
    }
}

/// `~/.bpm` holds catalogs and should be readable only by its owner. A wider
/// mode is reported on every command, but does not stop it.
fn warn_if_bpm_dir_is_shared() {
    let Ok(home) = catalog::home_dir() else {
        return;
    };
    let dir = home.join(".bpm");
    if let Some(mode) = perms::shared_mode(&dir) {
        eprintln!(
            "bpm: warning: {} is mode {mode:03o}, so other users may be able to read it; run `chmod 700 {}`",
            dir.display(),
            dir.display()
        );
    }
}
