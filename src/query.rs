//! Rendering for `bpm query` and `bpm sql`. No database access.

use crate::model::EntityRow;

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
