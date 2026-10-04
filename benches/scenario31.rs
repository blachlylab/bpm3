//! PRD §4.15 scenario 31: a synthetic catalog of 1,000,000 files and 5,000,000
//! metadata entries, queried through the same library calls `bpm query` makes.
//!
//!     cargo bench --bench scenario31                 # build if missing, then time
//!     cargo bench --bench scenario31 -- --rebuild    # rebuild the catalog first
//!     cargo bench --bench scenario31 -- --catalog PATH --json OUT
//!
//! The catalog is created by `Catalog::init`, so it carries every real
//! migration. Rows are then bulk-loaded with plain inserts into that schema,
//! because a million files cannot go through `bpm ingest` without a million
//! files on disk. Each timed query opens the catalog read-only, runs the tree
//! check, runs the query, and renders the table, as the CLI does. Nothing here
//! fails on a threshold; the PRD target is a few seconds on one workstation.

use std::path::{Path, PathBuf};
use std::time::Instant;

use bpm3::catalog::{Catalog, EntityQuery, FileQuery};
use bpm3::model::{Drift, NodeType, parse_selector};
use bpm3::query::{RenderFormat, render_entities, render_files};
use rusqlite::{Connection, params};
use uuid::Uuid;

const PROJECTS: usize = 10;
const CASES: usize = 10_000;
const SAMPLES_PER_CASE: usize = 2;
const RAW_PER_SAMPLE: usize = 2;
const ANALYSES: usize = 29_989;
const KEYS_PER_NODE: usize = 50;
const FILES: usize = 1_000_000;
/// Distinct values of generic key `kNN`, by `NN % 8`.
const CARDINALITY: [usize; 8] = [5, 20, 100, 1_000, 10_000, 100_000, 100_000, 100_000];
const RUNS: usize = 3;

struct Node {
    node_type: NodeType,
    id: String,
    parent: Option<String>,
    name: Option<String>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut catalog = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/scenario31.db");
    let mut json = None;
    let mut rebuild = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--catalog" => catalog = PathBuf::from(args.next().expect("--catalog PATH")),
            "--json" => json = Some(PathBuf::from(args.next().expect("--json PATH"))),
            "--rebuild" => rebuild = true,
            // cargo bench passes --bench to every target.
            "--bench" => {}
            other => panic!("unknown argument {other}"),
        }
    }
    if rebuild || !catalog.exists() {
        build(&catalog);
    }
    let results = time_queries(&catalog);
    if let Some(json) = json {
        std::fs::write(&json, serde_json::to_string_pretty(&results).unwrap()).unwrap();
        println!("wrote {}", json.display());
    }
}

fn build(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(name));
    }
    let started = Instant::now();
    Catalog::init(path, false).unwrap();
    let mut conn = Connection::open(path).unwrap();
    conn.execute_batch("PRAGMA synchronous = OFF; PRAGMA cache_size = -1000000;")
        .unwrap();
    let nodes = tree();
    let tx = conn.transaction().unwrap();
    {
        let stamp = "2026-10-04T00:00:00.000Z";
        let mut meta = tx
            .prepare(
                "INSERT INTO entity_metadata (node_type, node_id, key, value, updated_at)
                 VALUES (?, ?, ?, ?, ?)",
            )
            .unwrap();
        let mut pairs = 0usize;
        for (ordinal, node) in nodes.iter().enumerate() {
            insert_node(&tx, node, stamp);
            let named = named_key(node, ordinal);
            let generic = KEYS_PER_NODE - usize::from(named.is_some());
            let mut rows: Vec<(String, String)> = (0..generic)
                .map(|key| {
                    let card = CARDINALITY[key % 8];
                    (
                        format!("k{key:02}"),
                        format!("v{}", (ordinal * 7 + key) % card),
                    )
                })
                .collect();
            rows.extend(named);
            rows.sort();
            for (key, value) in rows {
                meta.execute(params![node.node_type.slug(), node.id, key, value, stamp])
                    .unwrap();
                pairs += 1;
            }
        }
        println!("loaded {} entities, {pairs} metadata pairs", nodes.len());

        let raw: Vec<&Node> = nodes
            .iter()
            .filter(|node| node.node_type == NodeType::RawData)
            .collect();
        let mut file = tx
            .prepare(
                "INSERT INTO files (id, size_bytes, mtime, fingerprint, fingerprint_scheme, created_at)
                 VALUES (?, ?, ?, ?, 'xxh3-128-full', ?)",
            )
            .unwrap();
        let mut location = tx
            .prepare(
                "INSERT INTO file_locations (file_id, backend, uri, first_seen_at, last_seen_at,
                   last_size, last_mtime, presence, stat_state, digest_state)
                 VALUES (?, 'posix', ?, ?, ?, ?, ?, 'present', 'unchanged', 'match')",
            )
            .unwrap();
        let mut digest = tx
            .prepare(
                "INSERT INTO file_digests (file_id, algorithm, digest, source, generation, current, observed_at)
                 VALUES (?, 'blake3', ?, 'computed', 1, 1, ?)",
            )
            .unwrap();
        let mut link = tx
            .prepare(
                "INSERT INTO file_links (file_id, node_type, node_id, role) VALUES (?, 'raw_data', ?, 'data')",
            )
            .unwrap();
        for index in 0..FILES {
            let id = Uuid::now_v7().to_string();
            let size = 1_000 + index as i64;
            file.execute(params![id, size, stamp, format!("{index:032x}"), stamp])
                .unwrap();
            location
                .execute(params![
                    id,
                    format!("/bench/data/f{index:07}.fq.gz"),
                    stamp,
                    stamp,
                    size,
                    stamp
                ])
                .unwrap();
            digest
                .execute(params![id, digest_hex(index), stamp])
                .unwrap();
            link.execute(params![id, raw[index % raw.len()].id])
                .unwrap();
        }
        println!("loaded {FILES} files with a location, a digest, and a link each");
    }
    tx.commit().unwrap();
    conn.execute_batch("ANALYZE; PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    let size = std::fs::metadata(path).unwrap().len();
    println!(
        "built {} in {:.1} s, {:.2} GB",
        path.display(),
        started.elapsed().as_secs_f64(),
        size as f64 / 1e9
    );
}

/// The six-level tree, parents before children, about 100,000 entities.
fn tree() -> Vec<Node> {
    let new = |node_type, parent: Option<&Node>, name: Option<String>| Node {
        node_type,
        id: Uuid::now_v7().to_string(),
        parent: parent.map(|parent| parent.id.clone()),
        name,
    };
    let mut nodes = vec![new(NodeType::Program, None, Some("BENCH".into()))];
    for project in 0..PROJECTS {
        let program = &nodes[0];
        let node = new(
            NodeType::Project,
            Some(program),
            Some(format!("P{project:02}")),
        );
        nodes.push(node);
    }
    for case in 0..CASES {
        let node = new(NodeType::Case, Some(&nodes[1 + case % PROJECTS]), None);
        nodes.push(node);
    }
    let cases: Vec<usize> = (1 + PROJECTS..nodes.len()).collect();
    for &case in &cases {
        for _ in 0..SAMPLES_PER_CASE {
            let node = new(NodeType::Sample, Some(&nodes[case]), None);
            nodes.push(node);
        }
    }
    let samples: Vec<usize> = (cases[cases.len() - 1] + 1..nodes.len()).collect();
    for &sample in &samples {
        for _ in 0..RAW_PER_SAMPLE {
            let node = new(NodeType::RawData, Some(&nodes[sample]), None);
            nodes.push(node);
        }
    }
    let raw: Vec<usize> = (samples[samples.len() - 1] + 1..nodes.len()).collect();
    for analysis in 0..ANALYSES {
        let node = new(
            NodeType::Analysis,
            Some(&nodes[raw[analysis % raw.len()]]),
            None,
        );
        nodes.push(node);
    }
    nodes
}

/// The conventional key a type carries in the scenarios, if any.
fn named_key(node: &Node, ordinal: usize) -> Option<(String, String)> {
    match node.node_type {
        NodeType::Case => Some(("subject_id".into(), format!("CLL-{ordinal:06}"))),
        NodeType::Sample => Some((
            "sample_kind".into(),
            if ordinal % 4 == 0 { "slide" } else { "aliquot" }.into(),
        )),
        NodeType::RawData => Some(("assay".into(), ["WES", "WGS", "RNA"][ordinal % 3].into())),
        _ => None,
    }
}

fn insert_node(conn: &Connection, node: &Node, stamp: &str) {
    let (table, parent_column) = match node.node_type {
        NodeType::Program => ("programs", None),
        NodeType::Project => ("projects", Some("program_id")),
        NodeType::Case => ("cases", Some("project_id")),
        NodeType::Sample => ("samples", Some("case_id")),
        NodeType::RawData => ("raw_data", Some("sample_id")),
        NodeType::Analysis => ("analyses", Some("raw_data_id")),
    };
    let mut columns = vec!["id"];
    let mut values: Vec<&dyn rusqlite::ToSql> = vec![&node.id];
    if let Some(column) = parent_column {
        columns.push(column);
        values.push(node.parent.as_ref().unwrap());
    }
    if node.name.is_some() {
        columns.push("name");
        values.push(node.name.as_ref().unwrap());
    }
    columns.extend(["created_at", "updated_at"]);
    values.extend([&stamp as &dyn rusqlite::ToSql, &stamp]);
    let marks = vec!["?"; columns.len()].join(", ");
    conn.prepare_cached(&format!(
        "INSERT INTO {table} ({}) VALUES ({marks})",
        columns.join(", ")
    ))
    .unwrap()
    .execute(values.as_slice())
    .unwrap();
}

fn digest_hex(index: usize) -> String {
    format!("{index:064x}")
}

enum Query {
    Files(FileQuery),
    Entities(EntityQuery),
}

fn time_queries(path: &Path) -> serde_json::Value {
    let case_id: String = Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT node_id FROM entity_metadata WHERE key = 'subject_id' AND value = 'CLL-004243'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let selector = |raw: &str| parse_selector(raw).unwrap();
    let queries: Vec<(&str, Query)> = vec![
        (
            "files --under a Project",
            Query::Files(FileQuery {
                under: Some("/BENCH/P03".into()),
                ..FileQuery::default()
            }),
        ),
        (
            "entities --where subject_id:<one Case>",
            Query::Entities(EntityQuery {
                under: None,
                node_type: None,
                wheres: vec![selector("subject_id:CLL-004243")],
            }),
        ),
        (
            "files --under a Case",
            Query::Files(FileQuery {
                under: Some(case_id),
                ..FileQuery::default()
            }),
        ),
        (
            "entities --where k03:v42 (1,000 values)",
            Query::Entities(EntityQuery {
                under: None,
                node_type: None,
                wheres: vec![selector("k03:v42")],
            }),
        ),
        (
            "entities --under a Project --type case",
            Query::Entities(EntityQuery {
                under: Some("/BENCH/P03".into()),
                node_type: Some(NodeType::Case),
                wheres: Vec::new(),
            }),
        ),
        (
            "files --digest blake3:<one file>",
            Query::Files(FileQuery {
                digest: Some(("blake3".into(), digest_hex(424_242))),
                ..FileQuery::default()
            }),
        ),
        (
            "files --unlinked",
            Query::Files(FileQuery {
                unlinked: true,
                ..FileQuery::default()
            }),
        ),
        (
            "files --drift missing",
            Query::Files(FileQuery {
                drift: vec![Drift::Missing],
                ..FileQuery::default()
            }),
        ),
    ];
    let mut out = serde_json::Map::new();
    println!(
        "{:44} {:>8} {:>10} {:>10}",
        "query", "rows", "first", "best"
    );
    for (label, query) in &queries {
        let mut times = Vec::new();
        let mut rows = 0;
        for _ in 0..RUNS {
            let started = Instant::now();
            // What `bpm query` does: open read-only, check the tree, query, render.
            let mut catalog = Catalog::open_read(path).unwrap();
            catalog.check_tree().unwrap();
            let rendered = match query {
                Query::Files(query) => {
                    let found = catalog.query_files(query).unwrap();
                    rows = found.len();
                    render_files(&found, RenderFormat::Table)
                }
                Query::Entities(query) => {
                    let found = catalog.query_entities(query).unwrap();
                    rows = found.len();
                    render_entities(&found, RenderFormat::Table)
                }
            };
            std::hint::black_box(rendered);
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        let best = times.iter().copied().fold(f64::INFINITY, f64::min);
        println!("{label:44} {rows:>8} {:>8.1}ms {:>8.1}ms", times[0], best);
        out.insert(
            label.to_string(),
            serde_json::json!({"rows": rows, "first_ms": round(times[0]), "best_ms": round(best)}),
        );
    }
    serde_json::Value::Object(out)
}

fn round(ms: f64) -> f64 {
    (ms * 10.0).round() / 10.0
}
