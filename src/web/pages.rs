//! The read-only pages. Each handler reads one snapshot of the catalog, turns
//! the library's rows into display strings, and renders a template. Templates
//! do no formatting of their own.

use askama::Template;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use uuid::Uuid;

use super::{AppState, Page, WebError, read};
use crate::catalog::{EntityQuery, FileQuery};
use crate::model::{
    DigestRow, Drift, EntityRow, FileRef, FileRow, LinkRow, NodeType, Paged, Window, parse_digest,
    parse_selector,
};

/// Rows per page in every paged table.
const PAGE_SIZE: usize = 100;

/// Selector inputs shown on the search form.
const WHERE_INPUTS: usize = 3;

// ----- display values ------------------------------------------------------

struct EntityView {
    href: String,
    type_label: &'static str,
    /// The path of a Program or Project, the UUID of anything else.
    label: String,
    id: String,
    metadata: Vec<(String, String)>,
}

fn entity_view(row: &EntityRow) -> EntityView {
    EntityView {
        href: format!("/entities/{}", row.id),
        type_label: row.node_type.label(),
        label: row.path.clone().unwrap_or_else(|| row.id.to_string()),
        id: row.id.to_string(),
        metadata: row
            .metadata
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    }
}

struct LinkView {
    href: String,
    label: String,
    role: String,
}

fn link_view(link: &LinkRow) -> LinkView {
    LinkView {
        href: format!("/entities/{}", link.node_id),
        label: format!("{} {}", link.node_type.label(), link.node_id),
        role: link.role.clone(),
    }
}

struct FileView {
    href: String,
    id: String,
    size: String,
    size_exact: String,
    drift: Vec<&'static str>,
    locations: Vec<String>,
    links: Vec<LinkView>,
}

fn file_view(row: &FileRow) -> FileView {
    FileView {
        href: format!("/files/{}", row.id),
        id: row.id.to_string(),
        size: row.size.map(bytes).unwrap_or_default(),
        size_exact: row.size.map(thousands).unwrap_or_default(),
        drift: slugs(&row.drift()),
        locations: row
            .locations
            .iter()
            .map(|location| location.uri.clone())
            .collect(),
        links: row.links.iter().map(link_view).collect(),
    }
}

fn slugs(states: &[Drift]) -> Vec<&'static str> {
    states.iter().map(|state| state.slug()).collect()
}

/// Previous and next links for one paged table. The links keep every other
/// query parameter, so they are plain URLs a browser or hx-boost can follow.
struct Pager {
    page: usize,
    pages: usize,
    prev: Option<String>,
    next: Option<String>,
    summary: String,
}

fn pager(total: usize, page: usize, path: &str, params: &[(String, String)], key: &str) -> Pager {
    let pages = total.div_ceil(PAGE_SIZE).max(1);
    let page = page.clamp(1, pages);
    let link = |to: usize| {
        let mut query = form_urlencoded::Serializer::new(String::new());
        for (name, value) in params.iter().filter(|(name, _)| name != key) {
            query.append_pair(name, value);
        }
        if to > 1 {
            query.append_pair(key, &to.to_string());
        }
        let query = query.finish();
        if query.is_empty() {
            path.to_string()
        } else {
            format!("{path}?{query}")
        }
    };
    let first = (page - 1) * PAGE_SIZE + 1;
    let last = (page * PAGE_SIZE).min(total);
    let summary = match total {
        0 => "None.".to_string(),
        _ if pages == 1 => format!("{} in all.", thousands(total as i64)),
        _ => format!(
            "{}–{} of {}.",
            thousands(first as i64),
            thousands(last as i64),
            thousands(total as i64)
        ),
    };
    Pager {
        page,
        pages,
        prev: (page > 1).then(|| link(page - 1)),
        next: (page < pages).then(|| link(page + 1)),
        summary,
    }
}

#[derive(Template)]
#[template(path = "entity_rows.html")]
struct EntityRows {
    rows: Vec<EntityView>,
    pager: Pager,
}

#[derive(Template)]
#[template(path = "file_rows.html")]
struct FileRows {
    rows: Vec<FileView>,
    pager: Pager,
}

fn href(path: &str, pairs: &[(&str, &str)]) -> String {
    let mut query = form_urlencoded::Serializer::new(String::new());
    for (name, value) in pairs {
        query.append_pair(name, value);
    }
    format!("{path}?{}", query.finish())
}

fn bytes(size: i64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    if n < 0 { format!("-{out}") } else { out }
}

/// The query string as ordered pairs. Repeated names (`where`, `drift`) stay
/// repeated.
fn pairs(raw: Option<String>) -> Vec<(String, String)> {
    form_urlencoded::parse(raw.unwrap_or_default().as_bytes())
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect()
}

fn param<'a>(params: &'a [(String, String)], name: &str) -> &'a str {
    params
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or("")
}

fn page_number(params: &[(String, String)], name: &str) -> usize {
    param(params, name).parse().unwrap_or(1).max(1)
}

// ----- home ------------------------------------------------------------------

struct Count {
    name: String,
    count: String,
    href: String,
}

struct Bytes {
    name: &'static str,
    count: String,
    bytes: String,
    bytes_exact: String,
}

struct InfoView {
    path: String,
    catalog_id: String,
    label: String,
    created_at: String,
    schema_version: String,
}

#[derive(Template)]
#[template(path = "home.html")]
struct Home {
    info: InfoView,
    entities: Vec<Count>,
    files: String,
    bytes: String,
    bytes_exact: String,
    unlinked: String,
    linked: Vec<Bytes>,
    drift: Vec<Count>,
    backends: Vec<Count>,
    sample_kind: Vec<Count>,
    assay: Vec<Count>,
}

pub(super) async fn home(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, WebError> {
    let (info, summary) = read(&state, |catalog| Ok((catalog.info()?, catalog.summary()?))).await?;
    let metadata_counts = |values: &[(String, i64)], node_type: Option<NodeType>, key: &str| {
        values
            .iter()
            .map(|(value, count)| {
                let selector = format!("{key}:{value}");
                let mut query = vec![("kind", "entities"), ("where", selector.as_str())];
                if let Some(node_type) = node_type {
                    query.push(("type", node_type.slug()));
                }
                Count {
                    name: value.clone(),
                    count: thousands(*count),
                    href: href("/search", &query),
                }
            })
            .collect()
    };
    let body = Home {
        info: InfoView {
            path: info.path.display().to_string(),
            catalog_id: info.catalog_id,
            label: info.label,
            created_at: info.created_at,
            schema_version: info.schema_version,
        },
        entities: summary
            .entities
            .iter()
            .map(|(node_type, count)| Count {
                name: node_type.label().to_string(),
                count: thousands(*count),
                href: href(
                    "/search",
                    &[("kind", "entities"), ("type", node_type.slug())],
                ),
            })
            .collect(),
        files: thousands(summary.files),
        bytes: bytes(summary.bytes),
        bytes_exact: thousands(summary.bytes),
        unlinked: thousands(summary.unlinked),
        linked: summary
            .linked_by_type
            .iter()
            .map(|(node_type, count, total)| Bytes {
                name: node_type.label(),
                count: thousands(*count),
                bytes: bytes(*total),
                bytes_exact: thousands(*total),
            })
            .collect(),
        drift: summary
            .drift
            .iter()
            .map(|(state, count)| Count {
                name: state.slug().to_string(),
                count: thousands(*count),
                href: href("/search", &[("kind", "files"), ("drift", state.slug())]),
            })
            .collect(),
        backends: summary
            .locations_by_backend
            .iter()
            .map(|(backend, count)| Count {
                name: backend.clone(),
                count: thousands(*count),
                href: String::new(),
            })
            .collect(),
        sample_kind: metadata_counts(&summary.sample_kind, Some(NodeType::Sample), "sample_kind"),
        assay: metadata_counts(&summary.assay, None, "assay"),
    };
    Ok(Page::new("Home", body)?.respond(&headers, &state.label))
}

// ----- tree ------------------------------------------------------------------

struct ProgramView {
    href: String,
    label: String,
    projects: Vec<EntityView>,
}

#[derive(Template)]
#[template(path = "tree.html")]
struct Tree {
    programs: Vec<ProgramView>,
}

pub(super) async fn tree(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, WebError> {
    let (programs, projects) = read(&state, |catalog| {
        let of_type = |node_type| EntityQuery {
            under: None,
            node_type: Some(node_type),
            wheres: Vec::new(),
        };
        Ok((
            catalog.query_entities(&of_type(NodeType::Program))?,
            catalog.query_entities(&of_type(NodeType::Project))?,
        ))
    })
    .await?;
    let programs = programs
        .iter()
        .map(|program| {
            let prefix = format!("{}/", program.path.as_deref().unwrap_or_default());
            ProgramView {
                href: format!("/entities/{}", program.id),
                label: program.path.clone().unwrap_or_default(),
                projects: projects
                    .iter()
                    .filter(|project| {
                        project
                            .path
                            .as_deref()
                            .is_some_and(|path| path.starts_with(&prefix))
                    })
                    .map(entity_view)
                    .collect(),
            }
        })
        .collect();
    Ok(Page::new("Tree", Tree { programs })?.respond(&headers, &state.label))
}

// ----- one entity ------------------------------------------------------------

#[derive(Template)]
#[template(path = "entity.html")]
struct Entity {
    entity: EntityView,
    path: Option<String>,
    ancestors: Vec<EntityView>,
    /// `(key, value, search for entities with that pair)`.
    metadata: Vec<(String, String, String)>,
    children: String,
    files: String,
    files_under_href: String,
}

pub(super) async fn entity(
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, WebError> {
    let Ok(id) = Uuid::parse_str(&id) else {
        return Err(WebError::not_found(format!("{id} is not an entity id.")));
    };
    let params = pairs(query);
    let child_page = page_number(&params, "page");
    let file_page = page_number(&params, "files_page");
    let detail = read(&state, move |catalog| {
        catalog.entity_detail(
            &id.to_string(),
            Window::page(child_page, PAGE_SIZE),
            Window::page(file_page, PAGE_SIZE),
        )
    })
    .await?;
    let path = format!("/entities/{id}");
    let children = EntityRows {
        rows: detail.children.rows.iter().map(entity_view).collect(),
        pager: pager(detail.children.total, child_page, &path, &params, "page"),
    }
    .render()
    .map_err(WebError::render)?;
    let files = FileRows {
        rows: detail.files.rows.iter().map(file_view).collect(),
        pager: pager(detail.files.total, file_page, &path, &params, "files_page"),
    }
    .render()
    .map_err(WebError::render)?;
    let view = entity_view(&detail.entity);
    let title = format!("{} {}", view.type_label, view.label);
    let metadata = detail
        .entity
        .metadata
        .iter()
        .map(|(key, value)| {
            let selector = format!("{key}:{value}");
            let search = href("/search", &[("kind", "entities"), ("where", &selector)]);
            (key.clone(), value.clone(), search)
        })
        .collect();
    let body = Entity {
        path: detail.entity.path.clone(),
        ancestors: detail.ancestors.iter().map(entity_view).collect(),
        metadata,
        children: children.clone(),
        files: files.clone(),
        files_under_href: href("/search", &[("kind", "files"), ("under", &view.id)]),
        entity: view,
    };
    Ok(Page::new(title, body)?
        .fragment("children", children)
        .fragment("files", files)
        .respond(&headers, &state.label))
}

// ----- one file --------------------------------------------------------------

struct DigestView {
    wire: String,
    source: String,
    generation: i64,
    current: bool,
}

fn digest_view(digest: &DigestRow) -> DigestView {
    DigestView {
        wire: digest.wire(),
        source: digest.source.clone(),
        generation: digest.generation,
        current: digest.current,
    }
}

struct LocationView {
    backend: String,
    uri: String,
    drift: Vec<&'static str>,
    presence: String,
    stat_state: String,
    digest_state: String,
    last_seen_at: String,
}

#[derive(Template)]
#[template(path = "file.html")]
struct File {
    id: String,
    size: String,
    size_exact: String,
    mtime: String,
    fingerprint: Option<(String, String)>,
    drift: Vec<&'static str>,
    current: Vec<DigestView>,
    locations: Vec<LocationView>,
    links: Vec<LinkView>,
    history: Vec<DigestView>,
}

pub(super) async fn file(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, WebError> {
    let Ok(id) = Uuid::parse_str(&id) else {
        return Err(WebError::not_found(format!("{id} is not a file id.")));
    };
    let (row, history) = read(&state, move |catalog| {
        let file = FileRef::Id(id);
        Ok((catalog.file(&file)?, catalog.digest_history(&file)?))
    })
    .await?;
    let body = File {
        id: row.id.to_string(),
        size: row.size.map(bytes).unwrap_or_default(),
        size_exact: row.size.map(thousands).unwrap_or_default(),
        mtime: row.mtime.clone().unwrap_or_default(),
        fingerprint: row
            .fingerprint
            .as_ref()
            .map(|print| (print.scheme.clone(), print.hex.clone())),
        drift: slugs(&row.drift()),
        current: row.digests.iter().map(digest_view).collect(),
        locations: row
            .locations
            .iter()
            .map(|location| LocationView {
                backend: location.backend.clone(),
                uri: location.uri.clone(),
                drift: slugs(&location.drift()),
                presence: location.presence.clone(),
                stat_state: location.stat_state.clone(),
                digest_state: location.digest_state.clone(),
                last_seen_at: location.last_seen_at.clone(),
            })
            .collect(),
        links: row.links.iter().map(link_view).collect(),
        history: history.iter().map(digest_view).collect(),
    };
    Ok(Page::new(format!("File {}", row.id), body)?.respond(&headers, &state.label))
}

// ----- search ----------------------------------------------------------------

struct SearchForm {
    kind: &'static str,
    under: String,
    wheres: Vec<String>,
    /// `(slug, label, selected)`.
    types: Vec<(&'static str, &'static str, bool)>,
    role: String,
    digest: String,
    unlinked: bool,
    /// `(slug, checked)`.
    drift: Vec<(&'static str, bool)>,
}

#[derive(Template)]
#[template(path = "search.html")]
struct Search {
    form: SearchForm,
    results: String,
}

#[derive(Template)]
#[template(
    source = r#"<p class="error" role="alert">{{ message }}</p>"#,
    ext = "html"
)]
struct Message {
    message: String,
}

#[derive(Template)]
#[template(
    source = r#"<p class="muted">Choose filters and search. With no filters, every entity or file is listed.</p>"#,
    ext = "html"
)]
struct Prompt;

/// The search form and its results. Every filter is a query parameter, so a
/// search is a URL. With no parameters the form is shown alone.
pub(super) async fn search(
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Result<Response, WebError> {
    let params = pairs(query);
    let all = |name: &str| -> Vec<String> {
        params
            .iter()
            .filter(|(key, value)| key == name && !value.is_empty())
            .map(|(_, value)| value.clone())
            .collect()
    };
    let kind = if param(&params, "kind") == "files" {
        "files"
    } else {
        "entities"
    };
    let wheres = all("where");
    let drift_checked = all("drift");
    let node_type = param(&params, "type");
    let mut shown_wheres = wheres.clone();
    shown_wheres.resize(WHERE_INPUTS.max(wheres.len() + 1), String::new());
    let form = SearchForm {
        kind,
        under: param(&params, "under").to_string(),
        wheres: shown_wheres,
        types: NodeType::ALL
            .iter()
            .map(|t| (t.slug(), t.label(), t.slug() == node_type))
            .collect(),
        role: param(&params, "role").to_string(),
        digest: param(&params, "digest").to_string(),
        unlinked: param(&params, "unlinked") == "1",
        drift: Drift::ALL
            .iter()
            .map(|state| {
                (
                    state.slug(),
                    drift_checked.iter().any(|d| d == state.slug()),
                )
            })
            .collect(),
    };

    let (results, status) = if params.is_empty() {
        (Prompt.render().map_err(WebError::render)?, StatusCode::OK)
    } else {
        match run_search(&state, &params, &form, &wheres, &drift_checked, node_type).await? {
            Ok(html) => (html, StatusCode::OK),
            Err(message) => (
                Message { message }.render().map_err(WebError::render)?,
                StatusCode::BAD_REQUEST,
            ),
        }
    };
    Ok(Page::new(
        "Search",
        Search {
            form,
            results: results.clone(),
        },
    )?
    .fragment("results", results)
    .status(status)
    .respond(&headers, &state.label))
}

/// The results table, or a message about the filters. A catalog failure is
/// still an error page.
async fn run_search(
    state: &AppState,
    params: &[(String, String)],
    form: &SearchForm,
    wheres: &[String],
    drift: &[String],
    node_type: &str,
) -> Result<Result<String, String>, WebError> {
    let page = page_number(params, "page");
    let window = Window::page(page, PAGE_SIZE);
    let under = Some(form.under.trim().to_string()).filter(|under| !under.is_empty());
    let mut selectors = Vec::new();
    for raw in wheres {
        match parse_selector(raw.trim()) {
            Ok(selector) => selectors.push(selector),
            Err(err) => return Ok(Err(format!("{raw}: {err}"))),
        }
    }
    if form.kind == "entities" {
        let node_type = match node_type {
            "" => None,
            slug => match NodeType::parse(slug) {
                Some(node_type) => Some(node_type),
                None => return Ok(Err(format!("{slug} is not an entity type"))),
            },
        };
        let query = EntityQuery {
            under,
            node_type,
            wheres: selectors,
        };
        let found = read(state, move |catalog| {
            Ok(catalog.query_entities_page(&query, window))
        })
        .await?;
        let found: Paged<EntityRow> = match found {
            Ok(found) => found,
            Err(err) => return Ok(Err(err.to_string())),
        };
        let rows = EntityRows {
            rows: found.rows.iter().map(entity_view).collect(),
            pager: pager(found.total, page, "/search", params, "page"),
        };
        return Ok(Ok(rows.render().map_err(WebError::render)?));
    }
    if !selectors.is_empty() {
        return Ok(Err(
            "metadata selectors on files wait for file metadata; search entities, or use Under"
                .into(),
        ));
    }
    let digest = match form.digest.trim() {
        "" => None,
        raw => match parse_digest(raw) {
            Ok(digest) => Some(digest),
            Err(err) => return Ok(Err(err.to_string())),
        },
    };
    let mut states = Vec::new();
    for slug in drift {
        match Drift::parse(slug) {
            Some(state) => states.push(state),
            None => return Ok(Err(format!("{slug} is not a drift state"))),
        }
    }
    let query = FileQuery {
        under,
        role: Some(form.role.trim().to_string()).filter(|role| !role.is_empty()),
        unlinked: form.unlinked,
        drift: states,
        digest,
    };
    let found = read(state, move |catalog| {
        Ok(catalog.query_files_page(&query, window))
    })
    .await?;
    let found = match found {
        Ok(found) => found,
        Err(err) => return Ok(Err(err.to_string())),
    };
    let rows = FileRows {
        rows: found.rows.iter().map(file_view).collect(),
        pager: pager(found.total, page, "/search", params, "page"),
    };
    Ok(Ok(rows.render().map_err(WebError::render)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_sizes_read_well() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KiB");
        assert_eq!(bytes(500_999_500_000), "466.6 GiB");
    }

    #[test]
    fn pager_links_keep_the_other_parameters() {
        let params = vec![
            ("kind".to_string(), "files".to_string()),
            ("drift".to_string(), "missing".to_string()),
            ("drift".to_string(), "ok".to_string()),
            ("page".to_string(), "2".to_string()),
        ];
        let pager = pager(250, 2, "/search", &params, "page");
        assert_eq!(pager.pages, 3);
        assert_eq!(pager.summary, "101–200 of 250.");
        assert_eq!(
            pager.prev.as_deref(),
            Some("/search?kind=files&drift=missing&drift=ok")
        );
        assert_eq!(
            pager.next.as_deref(),
            Some("/search?kind=files&drift=missing&drift=ok&page=3")
        );
        let one = pager_for_one();
        assert!(one.prev.is_none() && one.next.is_none());
    }

    fn pager_for_one() -> Pager {
        pager(5, 1, "/x", &[], "page")
    }
}
