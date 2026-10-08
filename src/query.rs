//! Rendering for `bpm query` and `bpm sql`. No database access.

use crate::model::{EntityRow, FileRow, Summary};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFormat {
    Table,
    Csv,
    Json,
}

/// How much of a file row the table prints. CSV and JSON ignore this and
/// always print the full values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileLayout {
    Compact,
    Full,
}

pub fn render_entities(rows: &[EntityRow], format: RenderFormat) -> String {
    match format {
        RenderFormat::Table => render_table(
            &["node_type", "id", "path", "name", "metadata"],
            &rows.iter().map(entity_cells).collect::<Vec<_>>(),
        ),
        RenderFormat::Csv => render_csv(
            &["node_type", "id", "path", "name", "metadata"],
            &rows.iter().map(entity_cells).collect::<Vec<_>>(),
        ),
        RenderFormat::Json => {
            let items: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| {
                    serde_json::json!({
                        "node_type": row.node_type.slug(),
                        "id": row.id.to_string(),
                        "path": row.path,
                        "name": row.name,
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
/// The table follows `layout`. CSV and JSON stay full either way.
pub fn render_files(rows: &[FileRow], format: RenderFormat, layout: FileLayout) -> String {
    const FULL: [&str; 6] = ["id", "size", "digest", "drift", "locations", "links"];
    const COMPACT: [&str; 6] = ["id", "size", "digest", "drift", "name", "links"];
    match format {
        RenderFormat::Table => match layout {
            FileLayout::Full => {
                render_table(&FULL, &rows.iter().map(file_cells).collect::<Vec<_>>())
            }
            FileLayout::Compact => render_table(
                &COMPACT,
                &rows.iter().map(compact_cells).collect::<Vec<_>>(),
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
        row.name.clone().unwrap_or_default(),
        metadata_text(&row.metadata),
    ]
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

/// The same six columns, shortened to fit a terminal: the UUID's last group,
/// a 1–3 digit size, `algorithm:` plus the last 12 hex digits, the file name,
/// and the same shortening on each link's entity id.
fn compact_cells(row: &FileRow) -> Vec<String> {
    vec![
        uuid_tail(row.id),
        match row.size {
            Some(size) if size >= 0 => compact_size(size as u64),
            Some(size) => size.to_string(),
            None => String::new(),
        },
        digest_tail(row),
        drift_text(row),
        row.locations
            .iter()
            .map(|location| base_name(&location.uri).to_string())
            .collect::<Vec<_>>()
            .join("; "),
        row.links
            .iter()
            .map(|link| {
                format!(
                    "{} {} {}",
                    link.node_type.slug(),
                    uuid_tail(link.node_id),
                    link.role
                )
            })
            .collect::<Vec<_>>()
            .join("; "),
    ]
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

/// `algorithm:` and the last 12 hex digits. A shorter digest is shown whole.
fn digest_tail(row: &FileRow) -> String {
    let Some(digest) = row
        .digests
        .iter()
        .find(|digest| digest.algorithm == "blake3")
    else {
        return String::new();
    };
    let hex = digest.hex.as_str();
    let start = hex.len().saturating_sub(12);
    format!("{}:{}", digest.algorithm, &hex[start..])
}

/// The last group of a hyphenated UUID, 12 hex digits.
fn uuid_tail(id: uuid::Uuid) -> String {
    let text = id.to_string();
    match text.rsplit_once('-') {
        Some((_, tail)) => tail.to_string(),
        None => text,
    }
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
    let mut widths: Vec<usize> = headers.iter().map(|header| header.len()).collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            if index < widths.len() {
                widths[index] = widths[index].max(cell.len());
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
        let width = widths.get(index).copied().unwrap_or(cell.len());
        out.push_str(cell);
        if index + 1 < cells.len() {
            let pad = width.saturating_sub(cell.len());
            for _ in 0..pad {
                out.push(' ');
            }
        }
    }
    out.push('\n');
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
    use crate::model::{DigestRow, LinkRow, LocationRow, NodeType};
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

    #[test]
    fn compact_table_shortens_id_size_digest_name_and_links() {
        let rendered = render_files(&[sample()], RenderFormat::Table, FileLayout::Compact);
        assert_eq!(
            rendered,
            "\
id            size   digest               drift  name         links
446655440000  2 KiB  blake3:abababababab  ok     a.fq; b.bam  raw_data 555555555555 data
"
        );
    }

    #[test]
    fn full_table_keeps_the_wide_columns() {
        let row = sample();
        let rendered = render_files(&[row.clone()], RenderFormat::Table, FileLayout::Full);
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
        let csv = render_files(&[row.clone()], RenderFormat::Csv, FileLayout::Compact);
        assert!(csv.starts_with("id,size,digest,drift,locations,links\n"));
        assert!(csv.contains(&row.id.to_string()));
        assert!(csv.contains("/data/run/a.fq"));
        assert!(csv.contains("1536"));

        let json = render_files(&[row.clone()], RenderFormat::Json, FileLayout::Compact);
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
    fn a_missing_size_and_a_short_digest_stay_readable() {
        let mut row = sample();
        row.size = None;
        row.digests[0].hex = "abcd".into();
        row.locations.truncate(1);
        row.links.clear();
        let rendered = render_files(&[row], RenderFormat::Table, FileLayout::Compact);
        let line = rendered.lines().nth(1).unwrap();
        assert!(line.contains("blake3:abcd"));
        assert!(line.contains("a.fq"));
        assert!(!line.contains("KiB") && !line.contains(" B"));
    }

    #[test]
    fn a_negative_size_is_printed_as_the_integer() {
        let mut row = sample();
        row.size = Some(-3);
        let cells = compact_cells(&row);
        assert_eq!(cells[1], "-3");
    }
}
