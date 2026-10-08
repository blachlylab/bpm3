//! Stage 4 acceptance tests: PRD §4.15 scenario 29, the read-only web UI.
//! Each test runs `bpm serve` on a free port against its own catalog and
//! talks plain HTTP/1.0 to it.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "bpm3-s4-{label}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(fs::canonicalize(&path).unwrap())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

fn bpm(home: &Path, args: &[&str]) -> Run {
    let output = command(home).args(args).output().expect("spawn bpm");
    Run {
        code: output.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&output.stdout).into_owned(),
        err: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn command(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bpm"));
    cmd.current_dir(home)
        .env("HOME", home)
        .env_remove("BPM_CATALOG")
        .env_remove("BPM_HOST")
        .env_remove("BPM_PORT")
        .env_remove("BPM_TOKEN");
    cmd
}

fn ok(run: Run) -> Run {
    assert_eq!(run.code, 0, "stdout:\n{}\nstderr:\n{}", run.out, run.err);
    run
}

/// A catalog with a small tree, two files, a link, and one drifted file.
struct Fixture {
    scratch: Scratch,
    catalog: PathBuf,
    raw: String,
    case: String,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let scratch = Scratch::new(label);
        let home = scratch.0.clone();
        let catalog = home.join("cat.db");
        let cat = catalog.to_str().unwrap().to_string();
        ok(bpm(&home, &["init", &cat]));
        let run = |args: &[&str]| -> String {
            let mut full = vec!["--catalog", cat.as_str()];
            full.extend_from_slice(args);
            ok(bpm(&home, &full)).out.trim().to_string()
        };
        run(&["create", "program", "--name", "CLL"]);
        run(&[
            "create",
            "project",
            "--parent",
            "/CLL",
            "--name",
            "WES-relapse",
        ]);
        let case = run(&["create", "case", "--parent", "/CLL/WES-relapse"]);
        run(&["meta", "set", &case, "subject_id", "CLL-001"]);
        run(&["meta", "set", &case, "consent", "GRU"]);
        let sample = run(&["create", "sample", "--parent", &case]);
        run(&["meta", "set", &sample, "sample_kind", "aliquot"]);
        let raw = run(&["create", "raw_data", "--parent", &sample]);
        run(&["meta", "set", &raw, "assay", "WES"]);
        let data = home.join("data/run42");
        fs::create_dir_all(&data).unwrap();
        fs::write(data.join("S1_R1.fq.gz"), b"reads one\n").unwrap();
        fs::write(data.join("S1_R2.fq.gz"), b"reads two\n").unwrap();
        run(&["ingest", data.to_str().unwrap()]);
        run(&["scan", data.to_str().unwrap()]);
        run(&[
            "link",
            "--yes",
            "--to",
            &raw,
            data.join("S1_R1.fq.gz").to_str().unwrap(),
            "--role",
            "data",
        ]);
        fs::write(data.join("S1_R2.fq.gz"), b"READS TWO, changed\n").unwrap();
        run(&["scan", data.to_str().unwrap()]);
        Self {
            scratch,
            catalog,
            raw,
            case,
        }
    }

    fn home(&self) -> &Path {
        &self.scratch.0
    }

    fn cli(&self, args: &[&str]) -> String {
        let mut full = vec!["--catalog", self.catalog.to_str().unwrap()];
        full.extend_from_slice(args);
        ok(bpm(self.home(), &full)).out
    }

    fn serve(&self, args: &[&str], env: &[(&str, &str)]) -> Server {
        let mut cmd = command(self.home());
        cmd.args(["--catalog", self.catalog.to_str().unwrap(), "serve"])
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            cmd.env(key, value);
        }
        let mut child = cmd.spawn().unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let address = line
            .split("http://")
            .nth(1)
            .unwrap_or_else(|| panic!("no address in {line:?}"))
            .trim()
            .to_string();
        Server {
            child,
            address,
            banner: line,
        }
    }
}

struct Server {
    child: Child,
    address: String,
    banner: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Response {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Response {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

impl Server {
    fn request(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Response {
        let mut stream = TcpStream::connect(&self.address).unwrap();
        let mut request = format!("{method} {path} HTTP/1.0\r\nHost: {}\r\n", self.address);
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        if !body.is_empty() {
            request.push_str("Content-Type: application/x-www-form-urlencoded\r\n");
        }
        request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
        stream.write_all(request.as_bytes()).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
        let mut lines = head.lines();
        let status = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .unwrap();
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
            .collect();
        Response {
            status,
            headers,
            body: body.to_string(),
        }
    }

    fn get(&self, path: &str) -> Response {
        self.request("GET", path, &[], "")
    }

    fn page(&self, path: &str) -> String {
        let response = self.get(path);
        assert_eq!(response.status, 200, "{path}: {}", response.body);
        response.body
    }
}

/// Every href in a page that points at another page of the catalog.
fn links(html: &str, prefix: &str) -> Vec<String> {
    html.split("href=\"")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .filter(|href| href.starts_with(prefix))
        .map(|href| href.replace("&amp;", "&"))
        .collect()
}

#[test]
fn scenario_29_pages_match_the_cli() {
    let fixture = Fixture::new("pages");
    let server = fixture.serve(&["--port", "0"], &[]);
    assert!(
        server.address.starts_with("127.0.0.1:"),
        "{}",
        server.banner
    );

    // The tree shows the Program and Project the CLI shows.
    let tree = server.page("/tree");
    assert!(tree.contains("/CLL"), "{tree}");
    assert!(tree.contains("/CLL/WES-relapse"), "{tree}");
    let project = links(&tree, "/entities/")
        .into_iter()
        .find(|href| tree.contains(&format!("href=\"{href}\">/CLL/WES-relapse<")))
        .unwrap();
    let project_page = server.page(&project);
    assert!(project_page.contains(&fixture.case), "{project_page}");

    // The Case page shows its metadata, its ancestors, and its Sample.
    let case_page = server.page(&format!("/entities/{}", fixture.case));
    assert!(case_page.contains("subject_id") && case_page.contains("CLL-001"));
    assert!(case_page.contains("/CLL/WES-relapse"));

    // A file's drift state matches `bpm query files`.
    let cli: serde_json::Value =
        serde_json::from_str(&fixture.cli(&["query", "files", "--format", "json"])).unwrap();
    for file in cli.as_array().unwrap() {
        let id = file["id"].as_str().unwrap();
        let page = server.page(&format!("/files/{id}"));
        for state in file["drift"].as_array().unwrap() {
            let state = state.as_str().unwrap();
            assert!(
                page.contains(&format!("drift-{state}\">{state}<")),
                "{id} should show {state}: {page}"
            );
        }
        for location in file["locations"].as_array().unwrap() {
            assert!(page.contains(location["uri"].as_str().unwrap()));
        }
        if let Some(digest) = file["digests"].as_array().unwrap().first() {
            assert!(page.contains(digest["digest"].as_str().unwrap()));
        }
    }
    let drifted = cli
        .as_array()
        .unwrap()
        .iter()
        .find(|file| {
            file["drift"]
                .as_array()
                .unwrap()
                .iter()
                .any(|state| state == "digest_mismatch")
        })
        .expect("the fixture has a drifted file");
    let search = server.page("/search?kind=files&drift=digest_mismatch");
    assert!(search.contains(drifted["id"].as_str().unwrap()), "{search}");

    // Search answers the same selectors as the CLI.
    let found = server.page("/search?kind=entities&where=subject_id%3ACLL-001");
    assert!(found.contains(&fixture.case));
    let under = server.page(&format!(
        "/search?kind=files&under={}&role=data",
        fixture.case
    ));
    assert!(
        under.contains("S1_R1.fq.gz") && !under.contains("S1_R2.fq.gz"),
        "{under}"
    );
    let bad = server.get("/search?kind=entities&where=no-colon");
    assert_eq!(bad.status, 400);
    assert!(bad.body.contains("invalid selector"), "{}", bad.body);

    // The home page counts what `bpm query summary` counts.
    let home = server.page("/");
    let summary = fixture.cli(&["query", "summary", "--format", "csv"]);
    assert!(summary.contains("files,all,2,"));
    assert!(home.contains(">2<"), "{home}");
    assert!(home.contains(fixture.catalog.to_str().unwrap()));

    let missing = server.get("/entities/01900000-0000-7000-8000-000000000000");
    assert_eq!(missing.status, 404);
    assert_eq!(server.get("/files/not-a-uuid").status, 404);
    assert_eq!(server.get("/no/such/page").status, 404);
}

#[test]
fn scenario_29_the_ui_does_not_offer_a_write() {
    let fixture = Fixture::new("readonly");
    let server = fixture.serve(&["--port", "0"], &[]);
    let before = fs::read(&fixture.catalog).unwrap();
    let mut pages = vec![
        "/".to_string(),
        "/tree".into(),
        "/search".into(),
        "/search?kind=files".into(),
        format!("/entities/{}", fixture.case),
        format!("/entities/{}", fixture.raw),
    ];
    pages.extend(links(
        &server.page(&format!("/entities/{}", fixture.raw)),
        "/files/",
    ));
    for path in &pages {
        let body = server.page(path);
        let lower = body.to_lowercase();
        assert!(!lower.contains("method=\"post\""), "{path} has a POST form");
        assert!(!lower.contains("<script"), "{path} runs a script");
        // Every form is a GET to /search.
        for form in lower.split("<form").skip(1) {
            assert!(form.contains("method=\"get\""), "{path}: {form}");
        }
    }
    for path in &pages {
        for method in ["POST", "PUT", "PATCH", "DELETE"] {
            let response = server.request(method, path.split('?').next().unwrap(), &[], "x=1");
            assert_eq!(response.status, 405, "{method} {path}");
        }
    }
    // Without a token there is no login form, so even that POST is refused.
    let login = server.request("POST", "/login", &[], "token=x");
    assert_eq!(login.status, 303);
    assert_eq!(fs::read(&fixture.catalog).unwrap(), before);

    let headers = server.get("/");
    assert!(
        headers
            .header("content-security-policy")
            .is_some_and(|policy| policy.contains("default-src 'self'"))
    );
    assert_eq!(headers.header("cache-control"), Some("no-store"));
}

#[test]
fn scenario_29_port_flag_and_environment() {
    let fixture = Fixture::new("port");
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let by_env = fixture.serve(&[], &[("BPM_PORT", &port.to_string())]);
    assert_eq!(by_env.address, format!("127.0.0.1:{port}"));
    assert_eq!(by_env.get("/tree").status, 200);
    drop(by_env);

    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let flag_port = probe.local_addr().unwrap().port();
    drop(probe);
    // The flag wins over the environment.
    let by_flag = fixture.serve(
        &["--port", &flag_port.to_string()],
        &[("BPM_PORT", &port.to_string())],
    );
    assert_eq!(by_flag.address, format!("127.0.0.1:{flag_port}"));
}

#[test]
fn scenario_29_a_host_without_a_token_exits_before_binding() {
    let fixture = Fixture::new("host");
    let cat = fixture.catalog.to_str().unwrap();
    for (args, env) in [
        (vec!["--host", "0.0.0.0", "--port", "0"], vec![]),
        (vec!["--port", "0"], vec![("BPM_HOST", "0.0.0.0")]),
    ] {
        let mut cmd = command(fixture.home());
        cmd.args(["--catalog", cat, "serve"]).args(&args);
        for (key, value) in env {
            cmd.env(key, value);
        }
        let output = cmd.output().unwrap();
        assert!(!output.status.success());
        let err = String::from_utf8_lossy(&output.stderr);
        assert!(err.contains("token"), "{err}");
        assert!(output.stdout.is_empty(), "it must not report serving");
    }
}

#[test]
fn scenario_29_a_token_gates_the_catalog() {
    let fixture = Fixture::new("token");
    let secret = "correct horse battery staple";
    let server = fixture.serve(
        &["--host", "127.0.0.1", "--port", "0"],
        &[("BPM_TOKEN", secret)],
    );
    assert!(!server.banner.contains(secret));

    let blocked = server.get("/tree");
    assert_eq!(blocked.status, 303);
    assert_eq!(blocked.header("location"), Some("/login"));
    assert!(!blocked.body.contains("CLL"));
    let form = server.get("/login");
    assert_eq!(form.status, 200);
    assert!(form.body.contains("method=\"post\""));
    assert!(!form.body.contains("/CLL"));

    let wrong = server.request("POST", "/login", &[], "token=nope");
    assert_eq!(wrong.status, 401);
    assert!(wrong.header("set-cookie").is_none());
    assert!(wrong.body.contains("does not match"));

    let body = format!("token={}", form_encode(secret));
    let right = server.request("POST", "/login", &[], &body);
    assert_eq!(right.status, 303);
    let cookie = right.header("set-cookie").unwrap().to_string();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    let session = cookie.split(';').next().unwrap().to_string();

    let tree = server.request("GET", "/tree", &[("Cookie", &session)], "");
    assert_eq!(tree.status, 200);
    assert!(tree.body.contains("/CLL/WES-relapse"));
    let forged = server.request("GET", "/tree", &[("Cookie", "bpm_session=forged")], "");
    assert_eq!(forged.status, 303);
    // The stylesheet is served without a session.
    assert_eq!(server.get("/assets/app.css").status, 200);
}

#[test]
fn htmx_requests_get_the_content_or_one_fragment() {
    let fixture = Fixture::new("htmx");
    let server = fixture.serve(&["--port", "0"], &[]);
    let path = format!("/entities/{}", fixture.case);
    let full = server.page(&path);
    assert!(full.starts_with("<!doctype html>"));
    assert!(full.contains("id=\"children\""));

    let content = server.request("GET", &path, &[("HX-Request", "true")], "");
    assert!(!content.body.contains("<html"));
    assert!(content.body.contains("<h1>"));

    let children = server.request(
        "GET",
        &path,
        &[("HX-Request", "true"), ("HX-Target", "children")],
        "",
    );
    assert!(!children.body.contains("<h1>"));
    assert!(children.body.contains("Sample"), "{}", children.body);

    let results = server.request(
        "GET",
        "/search?kind=entities&type=case",
        &[("HX-Request", "true"), ("HX-Target", "results")],
        "",
    );
    assert!(!results.body.contains("<form"));
    assert!(results.body.contains(&fixture.case));

    // A boosted navigation still gets the whole page.
    let boosted = server.request(
        "GET",
        &path,
        &[("HX-Request", "true"), ("HX-Boosted", "true")],
        "",
    );
    assert!(boosted.body.starts_with("<!doctype html>"));
}

#[test]
fn serve_refuses_a_catalog_it_cannot_read() {
    let scratch = Scratch::new("nocat");
    let output = command(&scratch.0)
        .args([
            "--catalog",
            scratch.0.join("absent.db").to_str().unwrap(),
            "serve",
            "--port",
            "0",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("bpm init"));
}

fn form_encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => (byte as char).to_string(),
            b' ' => "+".to_string(),
            _ => format!("%{byte:02X}"),
        })
        .collect()
}
