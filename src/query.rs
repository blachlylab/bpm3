//! Rendering for `bpm query` and `bpm sql`. No database access.

use crate::model::{EntityRow, FileRow, Summary};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderFormat {
    Table,
    Csv,
    Json,
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
pub fn render_files(rows: &[FileRow], format: RenderFormat) -> String {
    const HEADERS: [&str; 6] = ["id", "size", "digest", "drift", "locations", "links"];
    match format {
        RenderFormat::Table => {
            render_table(&HEADERS, &rows.iter().map(file_cells).collect::<Vec<_>>())
        }
        RenderFormat::Csv => render_csv(&HEADERS, &rows.iter().map(file_cells).collect::<Vec<_>>()),
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
        row.digests
            .iter()
            .find(|digest| digest.algorithm == "blake3")
            .map(|digest| digest.wire())
            .unwrap_or_default(),
        row.drift()
            .iter()
            .map(|state| state.slug())
            .collect::<Vec<_>>()
            .join(","),
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
