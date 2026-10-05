//! `bpm serve`: the read-only web UI (Core v1, architecture overview §8).
//!
//! A traditional multi-page app. Every route is a GET that returns a whole HTML
//! page, links and GET forms move between pages, and nothing needs JavaScript.
//! The one POST is the token form on `/login`, which never writes the catalog.
//! Each request opens the catalog read-only for the length of that request.
//!
//! The HTMX seam, for when it is added: every page renders its `<main>`
//! content on its own and [`Page::respond`] wraps it in `layout.html`. A
//! request that carries `HX-Request` gets the content without the layout, and
//! one whose `HX-Target` names a fragment the page declared (`results`,
//! `children`, `files`) gets only that fragment. Those regions carry the same
//! ids in the full page. Adding HTMX is then vendoring the script under
//! `assets/vendor/`, loading it from `layout.html`, and adding `hx-*`
//! attributes; the handlers do not change.

mod pages;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use askama::Template;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use uuid::Uuid;

use crate::catalog::Catalog;
use crate::error::Error;

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 3000;
const SESSION_COOKIE: &str = "bpm_session";
const STYLESHEET: &str = include_str!("../../assets/app.css");

/// Where to listen, and the token, after the flag and the environment have
/// been read (clap reads `--host` before `BPM_HOST`, and so on).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeConfig {
    pub host: String,
    pub port: u16,
    pub token: Option<String>,
}

impl ServeConfig {
    /// Any host the operator sets, even a loopback one, needs a token. With no
    /// host set the server listens on `127.0.0.1` only. Empty values are unset.
    pub fn resolve(
        host: Option<String>,
        port: Option<u16>,
        token: Option<String>,
    ) -> Result<Self, Error> {
        let host = host.filter(|host| !host.is_empty());
        let token = token.filter(|token| !token.is_empty());
        if host.is_some() && token.is_none() {
            return Err(Error::Message(
                "--host or BPM_HOST needs a token: pass --token or set BPM_TOKEN".into(),
            ));
        }
        Ok(Self {
            host: host.unwrap_or_else(|| DEFAULT_HOST.into()),
            port: port.unwrap_or(DEFAULT_PORT),
            token,
        })
    }
}

#[derive(Clone)]
struct AppState {
    catalog: PathBuf,
    label: Arc<str>,
    token: Option<Arc<str>>,
    sessions: Arc<Mutex<HashSet<String>>>,
}

/// Check the catalog, bind, print the address, and serve until interrupted.
/// The token is never printed or logged.
pub fn serve(catalog: PathBuf, config: ServeConfig) -> Result<(), Error> {
    // A missing, foreign, older, or newer catalog is refused before binding.
    let label = Catalog::open_read(&catalog)?.info()?.label;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind((config.host.as_str(), config.port)).await?;
        let address = listener.local_addr()?;
        println!("bpm: serving {} at http://{address}", catalog.display());
        use std::io::Write;
        std::io::stdout().flush()?;
        let state = AppState {
            catalog,
            label: label.into(),
            token: config.token.map(Into::into),
            sessions: Arc::default(),
        };
        axum::serve(listener, router(state))
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        Ok(())
    })
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(pages::home))
        .route("/tree", get(pages::tree))
        .route("/entities/{id}", get(pages::entity))
        .route("/files/{id}", get(pages::file))
        .route("/search", get(pages::search))
        .route("/login", get(login_form).post(login))
        .route("/assets/app.css", get(stylesheet))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_session,
        ))
        .layer(middleware::map_response(security_headers))
        .with_state(state)
}

/// One rendered page: its `<main>` content and the fragments inside it that a
/// partial request may ask for by element id.
pub(crate) struct Page {
    title: String,
    main: String,
    fragments: Vec<(&'static str, String)>,
    status: StatusCode,
}

#[derive(Template)]
#[template(path = "layout.html")]
struct Layout<'a> {
    title: &'a str,
    label: &'a str,
    main: &'a str,
}

impl Page {
    fn new(title: impl Into<String>, main: impl Template) -> Result<Self, WebError> {
        Ok(Self {
            title: title.into(),
            main: main.render().map_err(WebError::render)?,
            fragments: Vec::new(),
            status: StatusCode::OK,
        })
    }

    fn fragment(mut self, id: &'static str, html: String) -> Self {
        self.fragments.push((id, html));
        self
    }

    fn status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    /// The whole page, or, for an HTMX request that is not a boosted
    /// navigation, the content or the one fragment it targets.
    fn respond(self, headers: &HeaderMap, label: &str) -> Response {
        let flag = |name: &str| headers.get(name).is_some_and(|value| value == "true");
        let body = if flag("hx-request") && !flag("hx-boosted") {
            let target = headers
                .get("hx-target")
                .and_then(|value| value.to_str().ok());
            match self
                .fragments
                .into_iter()
                .find(|(id, _)| Some(*id) == target)
            {
                Some((_, fragment)) => fragment,
                None => self.main,
            }
        } else {
            let layout = Layout {
                title: &self.title,
                label,
                main: &self.main,
            };
            match layout.render() {
                Ok(html) => html,
                Err(err) => return WebError::render(err).into_response(),
            }
        };
        (self.status, Html(body)).into_response()
    }
}

/// An error page. Catalog errors keep their own message, which the CLI prints
/// too; the UI does not parse it.
pub(crate) struct WebError {
    status: StatusCode,
    heading: &'static str,
    message: String,
}

impl WebError {
    fn render(err: askama::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            heading: "The page could not be rendered",
            message: err.to_string(),
        }
    }

    fn not_found(message: String) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            heading: "Not found",
            message,
        }
    }
}

impl From<Error> for WebError {
    fn from(err: Error) -> Self {
        let (status, heading) = match &err {
            Error::NotFound(_) | Error::FileNotFound(_) => (StatusCode::NOT_FOUND, "Not found"),
            Error::Ambiguous(_) | Error::Model(_) => (StatusCode::BAD_REQUEST, "Bad request"),
            Error::Inconsistent(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "The catalog is inconsistent",
            ),
            Error::SchemaNewer { .. } | Error::SchemaOlder { .. } => (
                StatusCode::SERVICE_UNAVAILABLE,
                "The catalog needs a different bpm",
            ),
            _ => (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong"),
        };
        Self {
            status,
            heading,
            message: err.to_string(),
        }
    }
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage<'a> {
    heading: &'a str,
    message: &'a str,
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let body = ErrorPage {
            heading: self.heading,
            message: &self.message,
        };
        match Page::new(self.heading, body) {
            Ok(page) => page.status(self.status).respond(&HeaderMap::new(), ""),
            Err(_) => (self.status, self.message).into_response(),
        }
    }
}

/// Run `read` against a fresh read-only connection on a blocking thread. The
/// tree check runs first, as it does for `bpm query`.
async fn read<T, F>(state: &AppState, read: F) -> Result<T, WebError>
where
    T: Send + 'static,
    F: FnOnce(&mut Catalog) -> Result<T, Error> + Send + 'static,
{
    let path = state.catalog.clone();
    tokio::task::spawn_blocking(move || {
        let mut catalog = Catalog::open_read(&path)?;
        catalog.check_tree()?;
        read(&mut catalog)
    })
    .await
    .map_err(|err| WebError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        heading: "Something went wrong",
        message: err.to_string(),
    })?
    .map_err(WebError::from)
}

async fn not_found() -> WebError {
    WebError::not_found("No page at this address.".into())
}

async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLESHEET,
    )
}

/// No JavaScript runs on these pages, so the policy allows nothing but this
/// origin's own stylesheet and forms. A vendored HTMX served from `/assets/`
/// fits the same policy.
async fn security_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    for (name, value) in [
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::CACHE_CONTROL, "no-store"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    response
}

/// With a token configured, every page but the login form and the stylesheet
/// needs a session cookie from `/login`.
async fn require_session(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if state.token.is_none() {
        return next.run(request).await;
    }
    let path = request.uri().path();
    if path == "/login" || path.starts_with("/assets/") || has_session(&state, request.headers()) {
        return next.run(request).await;
    }
    Redirect::to("/login").into_response()
}

fn has_session(state: &AppState, headers: &HeaderMap) -> bool {
    let sessions = state.sessions.lock().expect("session set");
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .any(|(name, value)| name == SESSION_COOKIE && sessions.contains(value))
}

#[derive(Template)]
#[template(path = "login.html")]
struct Login {
    failed: bool,
}

async fn login_form(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if state.token.is_none() {
        return Redirect::to("/").into_response();
    }
    match Page::new("Enter the token", Login { failed: false }) {
        Ok(page) => page.respond(&headers, &state.label),
        Err(err) => err.into_response(),
    }
}

/// Compare the posted token with the configured one. A match starts a
/// session; a mismatch shows the form again. This writes nothing to the
/// catalog.
async fn login(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(token) = &state.token else {
        return Redirect::to("/").into_response();
    };
    let offered = form_urlencoded::parse(&body)
        .find(|(name, _)| name == "token")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default();
    if !same(offered.as_bytes(), token.as_bytes()) {
        return match Page::new("Enter the token", Login { failed: true }) {
            Ok(page) => page
                .status(StatusCode::UNAUTHORIZED)
                .respond(&headers, &state.label),
            Err(err) => err.into_response(),
        };
    }
    let session = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    state
        .sessions
        .lock()
        .expect("session set")
        .insert(session.clone());
    let cookie = format!("{SESSION_COOKIE}={session}; HttpOnly; SameSite=Strict; Path=/");
    let mut response = Redirect::to("/").into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

/// Equal-length comparison that does not stop at the first differing byte.
fn same(offered: &[u8], expected: &[u8]) -> bool {
    if offered.len() != expected.len() {
        return false;
    }
    offered
        .iter()
        .zip(expected)
        .fold(0u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_loopback_on_3000_without_a_token() {
        let config = ServeConfig::resolve(None, None, None).unwrap();
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 3000);
        assert_eq!(config.token, None);
    }

    #[test]
    fn a_host_needs_a_token() {
        assert!(ServeConfig::resolve(Some("0.0.0.0".into()), None, None).is_err());
        assert!(ServeConfig::resolve(Some("127.0.0.1".into()), None, Some(String::new())).is_err());
        let config =
            ServeConfig::resolve(Some("0.0.0.0".into()), Some(8080), Some("s3cret".into()))
                .unwrap();
        assert_eq!((config.host.as_str(), config.port), ("0.0.0.0", 8080));
        // A token alone is allowed on the default host.
        assert!(ServeConfig::resolve(None, None, Some("s3cret".into())).is_ok());
    }

    #[test]
    fn token_comparison() {
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"abcd"));
    }
}
