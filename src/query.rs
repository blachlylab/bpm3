//! Rendering for `bpm query` and `bpm sql`. No database access.

use std::collections::HashSet;

use uuid::Uuid;

use crate::model::{Drift, EntityRow, FileRow, Summary, shown_id};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFormat {
    Table,
    Csv,
    Json,
}

/// How much of a row the table prints. CSV and JSON ignore this and always
/// print the full values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    Compact,
    Full,
}

/// One row per entity. The compact table shows short ids, and for a Case or
/// below the path of its Project with `/...`. `shared` holds the ids whose
/// short id another row also has; those are shown in full.
pub fn render_entities(
    rows: &[EntityRow],
    format: RenderFormat,
    layout: Layout,
    shared: &HashSet<Uuid>,
) -> String {
    const COMPACT: [&str; 5] = ["id", "type", "path", "parent", "metadata"];
    const FULL: [&str; 6] = ["node_type", "id", "path", "project", "parent", "metadata"];
    match format {
        RenderFormat::Table => match layout {
            Layout::Compact => render_compact(
                &COMPACT,
                &rows
                    .iter()
                    .map(|row| compact_entity_cells(row, shared))
                    .collect::<Vec<_>>(),
            ),
            Layout::Full => render_table(&FULL, &rows.iter().map(entity_cells).collect::<Vec<_>>()),
        },
        RenderFormat::Csv => render_csv(&FULL, &rows.iter().map(entity_cells).collect::<Vec<_>>()),
        RenderFormat::Json => {
            let items: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| {
                    serde_json::json!({
                        "node_type": row.node_type.slug(),
                        "id": row.id.to_string(),
                        "path": row.path,
                        "project": row.project,
                        "parent": row.parent.as_ref().map(|parent| serde_json::json!({
                            "node_type": parent.node_type.slug(),
                            "id": parent.id.to_string(),
                            "path": parent.path,
                        })),
                        "metadata": row.metadata,
                    })
                })
                .collect();
            format!(
                "{}\n",
                serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".into())
            )
        }
    }
}

/// One row per file. Table and CSV join several locations or links with `; `.
/// The table follows `layout`. CSV and JSON stay full either way. `shared`
/// is as for [`render_entities`], over file ids and linked entity ids.
pub fn render_files(
    rows: &[FileRow],
    format: RenderFormat,
    layout: Layout,
    shared: &HashSet<Uuid>,
) -> String {
    const FULL: [&str; 6] = ["id", "size", "digest", "drift", "locations", "links"];
    const COMPACT: [&str; 5] = ["id", "size", "drift", "links", "name"];
    match format {
        RenderFormat::Table => match layout {
            Layout::Full => render_table(&FULL, &rows.iter().map(file_cells).collect::<Vec<_>>()),
            Layout::Compact => render_compact(
                &COMPACT,
                &rows
                    .iter()
                    .map(|row| compact_cells(row, shared))
                    .collect::<Vec<_>>(),
            ),
        },
        RenderFormat::Csv => render_csv(&FULL, &rows.iter().map(file_cells).collect::<Vec<_>>()),
        RenderFormat::Json => {
            let items: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| {
                    serde_json::json!({
                        "id": row.id.to_string(),
                        "size": row.size,
                        "mtime": row.mtime,
                        "fingerprint": row.fingerprint.as_ref().map(|print| serde_json::json!({
                            "scheme": print.scheme,
                            "hex": print.hex,
                        })),
                        "digests": row.digests.iter().map(|digest| serde_json::json!({
                            "digest": digest.wire(),
                            "source": digest.source,
                            "generation": digest.generation,
                        })).collect::<Vec<_>>(),
                        "drift": row.drift().iter().map(|state| state.slug()).collect::<Vec<_>>(),
                        "locations": row.locations.iter().map(|location| serde_json::json!({
                            "backend": location.backend,
                            "uri": location.uri,
                            "presence": location.presence,
                            "stat_state": location.stat_state,
                            "digest_state": location.digest_state,
                            "drift": location.drift().iter().map(|state| state.slug()).collect::<Vec<_>>(),
                            "last_seen_at": location.last_seen_at,
                        })).collect::<Vec<_>>(),
                        "links": row.links.iter().map(|link| serde_json::json!({
                            "node_type": link.node_type.slug(),
                            "node_id": link.node_id.to_string(),
                            "role": link.role,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            format!(
                "{}\n",
                serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".into())
            )
        }
    }
}

/// `bpm query files --count`. One column, not the file rows.
pub fn render_count(count: i64, format: RenderFormat) -> String {
    let row = vec![count.to_string()];
    match format {
        RenderFormat::Table => render_table(&["count"], &[row]),
        RenderFormat::Csv => render_csv(&["count"], &[row]),
        RenderFormat::Json => format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({ "count": count }))
                .unwrap_or_else(|_| "{\"count\":0}".into())
        ),
    }
}

/// `bpm query summary`: one `section  name  count  bytes` row per figure.
pub fn render_summary(summary: &Summary, format: RenderFormat) -> String {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut push = |section: &str, name: &str, count: i64, bytes: Option<i64>| {
        rows.push(vec![
            section.to_string(),
            name.to_string(),
            count.to_string(),
            bytes.map(|bytes| bytes.to_string()).unwrap_or_default(),
        ]);
    };
    for (node_type, count) in &summary.entities {
        push("entities", node_type.slug(), *count, None);
    }
    push("files", "all", summary.files, Some(summary.bytes));
    push("files", "unlinked", summary.unlinked, None);
    for (node_type, count, bytes) in &summary.linked_by_type {
        push("linked_to", node_type.slug(), *count, Some(*bytes));
    }
    for (backend, count) in &summary.locations_by_backend {
        push("locations", backend, *count, None);
    }
    for (state, count) in &summary.drift {
        push("drift", state.slug(), *count, None);
    }
    for (value, count) in &summary.sample_kind {
        push("sample_kind", value, *count, None);
    }
    for (value, count) in &summary.assay {
        push("assay", value, *count, None);
    }
    for (role, count) in &summary.roles {
        push("role", role, *count, None);
    }
    const HEADERS: [&str; 4] = ["section", "name", "count", "bytes"];
    match format {
        RenderFormat::Table => render_table(&HEADERS, &rows),
        RenderFormat::Csv => render_csv(&HEADERS, &rows),
        RenderFormat::Json => {
            let items: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| {
                    serde_json::json!({
                        "section": row[0],
                        "name": row[1],
                        "count": row[2].parse::<i64>().unwrap_or(0),
                        "bytes": row[3].parse::<i64>().ok(),
                    })
                })
                .collect();
            format!(
                "{}\n",
                serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".into())
            )
        }
    }
}

pub fn render_sql(columns: &[String], rows: &[Vec<String>]) -> String {
    let headers: Vec<&str> = columns.iter().map(String::as_str).collect();
    render_table(&headers, rows)
}

fn entity_cells(row: &EntityRow) -> Vec<String> {
    vec![
        row.node_type.slug().to_string(),
        row.id.to_string(),
        row.path.clone().unwrap_or_default(),
        row.project.clone().unwrap_or_default(),
        row.parent
            .as_ref()
            .map(|parent| {
                parent
                    .path
                    .clone()
                    .unwrap_or_else(|| format!("{} {}", parent.node_type.slug(), parent.id))
            })
            .unwrap_or_default(),
        metadata_text(&row.metadata),
    ]
}

/// Short ids. A Program or Project shows its own path; anything below shows
/// its Project's path and `/...`, which is not an address, so it cannot be
/// pasted back as one. A parent is its path, or its short id; its type is
/// always the one above the row's. Paths are cut to [`PATH_WIDTH`], and the
/// metadata takes what is left.
fn compact_entity_cells(row: &EntityRow, shared: &HashSet<Uuid>) -> (Vec<String>, Elastic) {
    let path = match (&row.path, &row.project) {
        (Some(path), _) => fit_path(path),
        (None, Some(project)) => {
            format!("{}/...", cut_middle(project, PATH_WIDTH - "/...".len(), 50))
        }
        (None, None) => String::new(),
    };
    let parent = row
        .parent
        .as_ref()
        .map(|parent| match &parent.path {
            Some(path) => fit_path(path),
            None => shown_id(parent.id, shared),
        })
        .unwrap_or_default();
    (
        vec![
            shown_id(row.id, shared),
            row.node_type.slug().to_string(),
            path,
            parent,
        ],
        Elastic::Pairs(
            row.metadata
                .iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect(),
        ),
    )
}

fn fit_path(path: &str) -> String {
    cut_middle(path, PATH_WIDTH, 50)
}

fn file_cells(row: &FileRow) -> Vec<String> {
    vec![
        row.id.to_string(),
        row.size.map(|size| size.to_string()).unwrap_or_default(),
        blake3_wire(row),
        drift_text(row),
        row.locations
            .iter()
            .map(|location| location.uri.as_str())
            .collect::<Vec<_>>()
            .join("; "),
        row.links
            .iter()
            .map(|link| format!("{} {} {}", link.node_type.slug(), link.node_id, link.role))
            .collect::<Vec<_>>()
            .join("; "),
    ]
}

/// Shortened to fit [`COMPACT_WIDTH`]: the short id, a 1–3 digit size, the
/// worst drift state, a count of links, and one location's name with a count
/// of the other copies. The name takes what the other columns leave.
fn compact_cells(row: &FileRow, shared: &HashSet<Uuid>) -> (Vec<String>, Elastic) {
    let drift = row.drift();
    let worst = drift.iter().min_by_key(|state| severity(**state));
    let name = row
        .locations
        .first()
        .map(|location| base_name(&location.uri).to_string())
        .unwrap_or_default();
    (
        vec![
            shown_id(row.id, shared),
            match row.size {
                Some(size) if size >= 0 => compact_size(size as u64),
                Some(size) => size.to_string(),
                None => String::new(),
            },
            match worst {
                Some(state) if drift.len() > 1 => format!("{}+", state.slug()),
                Some(state) => state.slug().to_string(),
                None => String::new(),
            },
            match row.links.len() {
                0 => String::new(),
                count => count.to_string(),
            },
        ],
        Elastic::Name {
            name,
            copies: row.locations.len().saturating_sub(1),
        },
    )
}

/// Which drift state the compact table names when a file has several: the
/// one most in need of attention.
fn severity(state: Drift) -> u8 {
    match state {
        Drift::Missing => 0,
        Drift::DigestMismatch => 1,
        Drift::StatChanged => 2,
        Drift::Unverified => 3,
        Drift::Ok => 4,
    }
}

fn drift_text(row: &FileRow) -> String {
    row.drift()
        .iter()
        .map(|state| state.slug())
        .collect::<Vec<_>>()
        .join(",")
}

fn blake3_wire(row: &FileRow) -> String {
    row.digests
        .iter()
        .find(|digest| digest.algorithm == "blake3")
        .map(|digest| digest.wire())
        .unwrap_or_default()
}

fn base_name(uri: &str) -> &str {
    uri.rsplit('/').find(|part| !part.is_empty()).unwrap_or(uri)
}

/// One to three digits and a binary unit. Under 1000 bytes the unit is `B`.
/// 1000 bytes is `1 KiB`. Rounding is half up; a value that would show as
/// 1000 steps up a unit, through `EiB`.
fn compact_size(bytes: u64) -> String {
    const UNITS: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let bytes = u128::from(bytes);
    let mut unit = 0;
    let mut scale: u128 = 1;
    while unit + 1 < UNITS.len() {
        let rounded = (bytes + scale / 2) / scale;
        if rounded <= 999 {
            return format!("{rounded} {}", UNITS[unit]);
        }
        unit += 1;
        scale *= 1024;
    }
    let rounded = (bytes + scale / 2) / scale;
    format!("{rounded} {}", UNITS[unit])
}

fn metadata_text(metadata: &std::collections::BTreeMap<String, String>) -> String {
    metadata
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn render_table(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|header| display_width(header)).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            if index < widths.len() {
                widths[index] = widths[index].max(display_width(cell));
            }
        }
    }
    let mut out = String::new();
    push_row(&mut out, headers, &widths);
    for row in rows {
        let cells: Vec<&str> = row.iter().map(String::as_str).collect();
        push_row(&mut out, &cells, &widths);
    }
    out
}

fn push_row(out: &mut String, cells: &[&str], widths: &[usize]) {
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            out.push_str("  ");
        }
        out.push_str(cell);
        if index + 1 < cells.len() {
            let width = widths.get(index).copied().unwrap_or(0);
            for _ in display_width(cell)..width {
                out.push(' ');
            }
        }
    }
    out.push('\n');
}

/// The widest line a compact table prints, unless a shared short id has to be
/// shown in full.
pub const COMPACT_WIDTH: usize = 80;

/// The most a path takes in the compact entity table.
const PATH_WIDTH: usize = 16;

/// The last column of a compact table, fitted to the width the others leave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Elastic {
    /// A file name and how many other copies the file has. The name is cut in
    /// the middle, keeping more of the end; the count is never cut.
    Name { name: String, copies: usize },
    /// `key=value` pairs: as many whole pairs as fit, then `+N` for the rest.
    Pairs(Vec<String>),
    /// Cut at the end.
    Text(String),
}

impl Elastic {
    fn fit(&self, width: usize) -> String {
        match self {
            Self::Name { name, copies } => {
                let suffix = match copies {
                    0 => String::new(),
                    copies => format!(" (+{copies})"),
                };
                let room = width.saturating_sub(display_width(&suffix));
                format!("{}{suffix}", cut_middle(name, room, 70))
            }
            Self::Pairs(pairs) => {
                let more = |shown: usize| match pairs.len() - shown {
                    0 => String::new(),
                    rest => format!(" +{rest}"),
                };
                let mut out = String::new();
                for (index, pair) in pairs.iter().enumerate() {
                    let joined = if out.is_empty() {
                        pair.clone()
                    } else {
                        format!("{out}; {pair}")
                    };
                    if display_width(&joined) + display_width(&more(index + 1)) <= width {
                        out = joined;
                        continue;
                    }
                    if out.is_empty() {
                        // Not even one whole pair fits: cut the first.
                        let suffix = more(1);
                        let room = width.saturating_sub(display_width(&suffix));
                        return format!("{}{suffix}", cut_middle(pair, room, 60));
                    }
                    return format!("{out}{}", more(index));
                }
                out
            }
            Self::Text(text) => cut_end(text, width),
        }
    }
}

/// A table whose lines are at most [`COMPACT_WIDTH`] wide: every column but
/// the last as wide as its widest cell, and the last fitted into the rest.
pub fn render_compact(headers: &[&str], rows: &[(Vec<String>, Elastic)]) -> String {
    let fixed = headers.len() - 1;
    let mut widths: Vec<usize> = headers[..fixed]
        .iter()
        .map(|header| display_width(header))
        .collect();
    for (cells, _) in rows {
        for (width, cell) in widths.iter_mut().zip(cells) {
            *width = (*width).max(display_width(cell));
        }
    }
    let used: usize = widths.iter().sum::<usize>() + 2 * fixed;
    let room = COMPACT_WIDTH.saturating_sub(used);
    let mut out = String::new();
    push_row(&mut out, headers, &widths);
    for (cells, last) in rows {
        let last = last.fit(room);
        let mut line: Vec<&str> = cells.iter().map(String::as_str).collect();
        line.push(&last);
        push_row(&mut out, &line, &widths);
    }
    out
}

/// How many columns a terminal gives `text`. Combining marks take none, so an
/// accented letter counts once whether it is stored composed (`é`) or as `e`
/// and a mark, as macOS file names often are. Wide East Asian characters are
/// counted as one column, which is wrong, and not expected in a catalog.
pub fn display_width(text: &str) -> usize {
    text.chars().filter(|c| !is_combining(*c)).count()
}

fn is_combining(c: char) -> bool {
    matches!(
        c as u32,
        0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F
    )
}

/// `text` as letters with their combining marks, each one column wide.
fn clusters(text: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut start = 0;
    for (index, c) in text.char_indices() {
        if index > start && !is_combining(c) {
            out.push(&text[start..index]);
            start = index;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// `text` in at most `width` columns, its middle replaced by `…`. `tail` is the
/// percentage of the kept columns that come from the end.
pub fn cut_middle(text: &str, width: usize, tail: usize) -> String {
    let parts = clusters(text);
    if parts.len() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let kept = width - 1;
    let from_end = (kept * tail + 50) / 100;
    let from_start = kept - from_end;
    format!(
        "{}…{}",
        parts[..from_start].concat(),
        parts[parts.len() - from_end..].concat()
    )
}

/// `text` in at most `width` columns, ending in `…` when it was cut.
pub fn cut_end(text: &str, width: usize) -> String {
    let parts = clusters(text);
    if parts.len() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    format!("{}…", parts[..width - 1].concat())
}

fn render_csv(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    push_csv(&mut out, headers);
    for row in rows {
        let cells: Vec<&str> = row.iter().map(String::as_str).collect();
        push_csv(&mut out, &cells);
    }
    out
}

fn push_csv(out: &mut String, cells: &[&str]) {
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        if cell.contains([',', '"', '\n']) {
            out.push('"');
            out.push_str(&cell.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(cell);
        }
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::model::{DigestRow, EntityRow, LinkRow, LocationRow, NodeType, ParentRef};
    use uuid::Uuid;

    fn sample() -> FileRow {
        FileRow {
            id: Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap(),
            size: Some(1536),
            mtime: None,
            fingerprint: None,
            digests: vec![DigestRow {
                algorithm: "blake3".into(),
                hex: "ab".repeat(32),
                source: "scan".into(),
                generation: 1,
                current: true,
            }],
            locations: vec![
                LocationRow {
                    backend: "posix".into(),
                    uri: "/data/run/a.fq".into(),
                    presence: "present".into(),
                    stat_state: "unchanged".into(),
                    digest_state: "match".into(),
                    last_seen_at: "2026-01-01T00:00:00Z".into(),
                },
                LocationRow {
                    backend: "s3".into(),
                    uri: "s3://bucket/dir/b.bam".into(),
                    presence: "present".into(),
                    stat_state: "unchanged".into(),
                    digest_state: "match".into(),
                    last_seen_at: "2026-01-01T00:00:00Z".into(),
                },
            ],
            links: vec![LinkRow {
                node_type: NodeType::RawData,
                node_id: Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
                role: "data".into(),
            }],
        }
    }

    fn entities() -> Vec<EntityRow> {
        let program = Uuid::parse_str("01a17f62-d423-74d7-ae78-c9620b896c52").unwrap();
        let project = Uuid::parse_str("01a17f62-d42b-736f-ae7f-87231f3eb357").unwrap();
        let case = Uuid::parse_str("01a17f62-d434-702a-b03b-95cf52f8ee52").unwrap();
        let sample = Uuid::parse_str("01a17f62-d442-71f8-a6cf-bf9cceb2a884").unwrap();
        let row = |node_type, id, path: Option<&str>, parent: Option<ParentRef>| EntityRow {
            node_type,
            id,
            path: path.map(String::from),
            name: None,
            parent,
            project: None,
            metadata: BTreeMap::new(),
        };
        let mut rows = vec![
            row(NodeType::Program, program, Some("/CLL"), None),
            row(
                NodeType::Project,
                project,
                Some("/CLL/WES"),
                Some(ParentRef {
                    node_type: NodeType::Program,
                    id: program,
                    path: Some("/CLL".into()),
                }),
            ),
            row(
                NodeType::Case,
                case,
                None,
                Some(ParentRef {
                    node_type: NodeType::Project,
                    id: project,
                    path: Some("/CLL/WES".into()),
                }),
            ),
            row(
                NodeType::Sample,
                sample,
                None,
                Some(ParentRef {
                    node_type: NodeType::Case,
                    id: case,
                    path: None,
                }),
            ),
        ];
        rows[2].project = Some("/CLL/WES".into());
        rows[3].project = Some("/CLL/WES".into());
        rows[2]
            .metadata
            .insert("subject_id".into(), "CLL-001".into());
        rows[3]
            .metadata
            .insert("sample_kind".into(), "tumor".into());
        rows
    }

    #[test]
    fn compact_entity_table_shows_short_ids_and_where_each_entity_sits() {
        let rendered = render_entities(
            &entities(),
            RenderFormat::Table,
            Layout::Compact,
            &HashSet::new(),
        );
        assert_eq!(
            rendered,
            "\
id            type     path          parent        metadata
c9620b896c52  program  /CLL                        
87231f3eb357  project  /CLL/WES      /CLL          
95cf52f8ee52  case     /CLL/WES/...  /CLL/WES      subject_id=CLL-001
bf9cceb2a884  sample   /CLL/WES/...  95cf52f8ee52  sample_kind=tumor
"
        );
    }

    #[test]
    fn a_shared_short_id_is_shown_in_full() {
        let rows = entities();
        let case = rows[2].id;
        let rendered = render_entities(
            &rows,
            RenderFormat::Table,
            Layout::Compact,
            &HashSet::from([case]),
        );
        // The row's own id and the sample's parent are both shown in full.
        assert!(rendered.contains(&format!("{case}  case")), "{rendered}");
        assert!(rendered.contains(&format!("...  {case}")), "{rendered}");
    }

    #[test]
    fn full_entity_table_csv_and_json_keep_full_ids() {
        let rows = entities();
        let sample = &rows[3];
        let full = render_entities(&rows, RenderFormat::Table, Layout::Full, &HashSet::new());
        assert_eq!(
            full.lines()
                .next()
                .unwrap()
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["node_type", "id", "path", "project", "parent", "metadata"]
        );
        let last = full.lines().last().unwrap();
        assert!(last.contains(&sample.id.to_string()), "{last}");
        assert!(last.contains(&format!("case {}", rows[2].id)), "{last}");

        let csv = render_entities(&rows, RenderFormat::Csv, Layout::Compact, &HashSet::new());
        assert!(csv.starts_with("node_type,id,path,project,parent,metadata\n"));
        assert!(csv.contains(&sample.id.to_string()));

        let json: serde_json::Value = serde_json::from_str(&render_entities(
            &rows,
            RenderFormat::Json,
            Layout::Compact,
            &HashSet::new(),
        ))
        .unwrap();
        assert!(json[0].get("name").is_none());
        assert_eq!(json[0]["path"], "/CLL");
        assert_eq!(json[3]["path"], serde_json::Value::Null);
        assert_eq!(json[3]["project"], "/CLL/WES");
        assert_eq!(json[3]["parent"]["node_type"], "case");
        assert_eq!(json[3]["parent"]["id"], rows[2].id.to_string());
        assert_eq!(json[3]["metadata"]["sample_kind"], "tumor");
    }

    #[test]
    fn compact_table_shows_worst_drift_a_link_count_and_one_name() {
        let rendered = render_files(
            &[sample()],
            RenderFormat::Table,
            Layout::Compact,
            &HashSet::new(),
        );
        assert_eq!(
            rendered,
            "\
id            size   drift  links  name
446655440000  2 KiB  ok     1      a.fq (+1)
"
        );
    }

    /// Every line of `table` is at most [`COMPACT_WIDTH`] columns.
    fn assert_fits(table: &str) {
        for line in table.lines() {
            assert!(
                display_width(line) <= COMPACT_WIDTH,
                "{} columns:\n{line}",
                display_width(line)
            );
        }
    }

    #[test]
    fn long_names_are_cut_in_the_middle_to_fit_eighty_columns() {
        let mut row = sample();
        row.size = Some(4 << 30);
        row.locations[0].uri = "/data/run/SAMPLE_0042_S12_L001_R1_001.fastq.gz".into();
        let mut long = row.clone();
        long.locations[0].uri = format!("/data/{}.bam", "CLL-001_tumor_WES".repeat(6));
        long.locations[0].presence = "missing".into();
        let rendered = render_files(
            &[row, long],
            RenderFormat::Table,
            Layout::Compact,
            &HashSet::new(),
        );
        assert_fits(&rendered);
        let lines: Vec<&str> = rendered.lines().collect();
        // The usual Illumina name fits whole.
        assert!(
            lines[1].ends_with("SAMPLE_0042_S12_L001_R1_001.fastq.gz (+1)"),
            "{rendered}"
        );
        // A long one keeps its start and more of its end, and the copy count.
        assert!(lines[2].contains("CLL-001_tum…"), "{rendered}");
        assert!(lines[2].ends_with("WES.bam (+1)"), "{rendered}");
        assert!(lines[2].contains("missing+"), "{rendered}");
        assert_eq!(display_width(lines[2]), COMPACT_WIDTH, "{rendered}");
    }

    #[test]
    fn accented_names_count_one_column_per_letter_in_either_form() {
        let composed = "R\u{e9}sum\u{e9}_\u{c5}ngstr\u{f6}m.fq";
        let decomposed = "Re\u{301}sume\u{301}_A\u{30a}ngstro\u{308}m.fq";
        assert_eq!(display_width(composed), 18);
        assert_eq!(display_width(decomposed), 18);
        // A cut never strips a mark from its letter.
        for width in 2..18 {
            let cut = cut_middle(decomposed, width, 70);
            assert_eq!(display_width(&cut), width, "{cut}");
            assert!(
                !cut.contains("…\u{301}") && !cut.contains("…\u{30a}") && !cut.contains("…\u{308}"),
                "{cut}"
            );
            let end = cut_end(decomposed, width);
            assert_eq!(display_width(&end), width, "{end}");
        }
        let mut row = sample();
        row.locations.truncate(1);
        row.locations[0].uri = format!("/data/{decomposed}");
        let mut other = row.clone();
        other.locations[0].uri = format!("/data/{composed}");
        let rendered = render_files(
            &[row, other],
            RenderFormat::Table,
            Layout::Compact,
            &HashSet::new(),
        );
        let widths: Vec<usize> = rendered.lines().map(display_width).collect();
        assert_eq!(widths[1], widths[2], "{rendered}");
    }

    #[test]
    fn long_paths_and_metadata_fit_eighty_columns() {
        let mut rows = entities();
        let program = "Chronic-Lymphocytic-Leukemia";
        let project = "Whole-Exome-Relapse-Cohort-2026";
        rows[0].path = Some(format!("/{program}"));
        rows[1].path = Some(format!("/{program}/{project}"));
        rows[1].parent.as_mut().unwrap().path = Some(format!("/{program}"));
        for row in &mut rows[2..] {
            row.project = Some(format!("/{program}/{project}"));
        }
        rows[2].parent.as_mut().unwrap().path = Some(format!("/{program}/{project}"));
        rows[2].metadata.insert("consent".into(), "GRU".into());
        rows[2]
            .metadata
            .insert("embargo_until".into(), "2027-01-01".into());
        rows[3].metadata.insert(
            "tissue".into(),
            "bone_marrow_aspirate_from_left_iliac_crest".into(),
        );
        let rendered =
            render_entities(&rows, RenderFormat::Table, Layout::Compact, &HashSet::new());
        assert_fits(&rendered);
        let lines: Vec<&str> = rendered.lines().collect();
        // Paths are cut to sixteen, and a lower entity keeps the `/...`.
        assert!(lines[2].contains("  /Chroni…ort-2026  "), "{rendered}");
        assert!(lines[3].contains("  /Chro…t-2026/...  "), "{rendered}");
        // A parent below a Project is its short id alone.
        assert!(lines[4].contains("  95cf52f8ee52  "), "{rendered}");
        // Whole pairs while they fit, then a count of the rest.
        assert!(lines[3].ends_with("consent=GRU +2"), "{rendered}");
        assert!(lines[4].ends_with("sample_kind=tumor +1"), "{rendered}");
    }

    #[test]
    fn a_pair_too_long_for_the_column_is_cut() {
        let pairs = Elastic::Pairs(vec![
            "subject_id=CLL-0001-relapse-second-biopsy".into(),
            "consent=GRU".into(),
        ]);
        let fitted = pairs.fit(20);
        assert_eq!(display_width(&fitted), 20, "{fitted}");
        assert!(
            fitted.starts_with("subj") && fitted.ends_with(" +1"),
            "{fitted}"
        );
        assert_eq!(
            Elastic::Pairs(vec!["a=1".into(), "b=2".into()]).fit(20),
            "a=1; b=2"
        );
        assert_eq!(Elastic::Text("abcdef".into()).fit(4), "abc…");
        assert_eq!(cut_middle("abcdef", 0, 50), "");
    }

    #[test]
    fn full_table_keeps_the_wide_columns() {
        let row = sample();
        let rendered = render_files(
            &[row.clone()],
            RenderFormat::Table,
            Layout::Full,
            &HashSet::new(),
        );
        let line = rendered.lines().nth(1).unwrap();
        assert!(line.contains(&row.id.to_string()));
        assert!(line.contains(" 1536 "));
        assert!(line.contains(&format!("blake3:{}", "ab".repeat(32))));
        assert!(line.contains("/data/run/a.fq; s3://bucket/dir/b.bam"));
        assert!(line.contains("raw_data 11111111-2222-3333-4444-555555555555 data"));
        assert_eq!(
            rendered
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .collect::<Vec<_>>(),
            ["id", "size", "digest", "drift", "locations", "links"]
        );
    }

    #[test]
    fn csv_and_json_stay_full_when_the_table_is_compact() {
        let row = sample();
        let csv = render_files(
            &[row.clone()],
            RenderFormat::Csv,
            Layout::Compact,
            &HashSet::new(),
        );
        assert!(csv.starts_with("id,size,digest,drift,locations,links\n"));
        assert!(csv.contains(&row.id.to_string()));
        assert!(csv.contains("/data/run/a.fq"));
        assert!(csv.contains("1536"));

        let json = render_files(
            &[row.clone()],
            RenderFormat::Json,
            Layout::Compact,
            &HashSet::new(),
        );
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["id"], row.id.to_string());
        assert_eq!(parsed[0]["size"], 1536);
        assert_eq!(parsed[0]["locations"][0]["uri"], "/data/run/a.fq");
    }

    #[test]
    fn compact_size_is_one_to_three_digits() {
        let cases = [
            (0, "0 B"),
            (999, "999 B"),
            (1000, "1 KiB"),
            (1023, "1 KiB"),
            (1024, "1 KiB"),
            (1535, "1 KiB"),
            (1536, "2 KiB"),
            (999 * 1024, "999 KiB"),
            (999 * 1024 + 512, "1 MiB"),
            (1024 * 1024, "1 MiB"),
            (1024u64.pow(3), "1 GiB"),
            (1024u64.pow(4), "1 TiB"),
            (1024u64.pow(5), "1 PiB"),
            (1024u64.pow(6), "1 EiB"),
        ];
        for (bytes, want) in cases {
            assert_eq!(compact_size(bytes), want, "{bytes}");
            assert!(
                want.split_once(' ').unwrap().0.len() <= 3,
                "{want} has more than three digits"
            );
        }
    }

    #[test]
    fn a_missing_size_and_no_links_stay_readable() {
        let mut row = sample();
        row.size = None;
        row.locations.truncate(1);
        row.links.clear();
        let rendered = render_files(
            &[row],
            RenderFormat::Table,
            Layout::Compact,
            &HashSet::new(),
        );
        let line = rendered.lines().nth(1).unwrap();
        assert!(line.ends_with("a.fq"), "{line}");
        assert!(!line.contains("KiB") && !line.contains(" B"), "{line}");
    }

    #[test]
    fn a_negative_size_is_printed_as_the_integer() {
        let mut row = sample();
        row.size = Some(-3);
        let (cells, _) = compact_cells(&row, &HashSet::new());
        assert_eq!(cells[1], "-3");
    }
}
