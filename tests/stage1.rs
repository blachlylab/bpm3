//! Stage 1 acceptance tests: PRD §4.15 scenarios 1–9, 25, 27, 30, and 32.
//! Each test uses its own HOME and its own catalog files.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use rusqlite::{Connection, TransactionBehavior};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "bpm3-{label}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
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

fn bpm(home: &Path, cwd: &Path, args: &[&str]) -> Run {
    bpm_env(home, cwd, args, &[])
}

fn bpm_env(home: &Path, cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> Run {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bpm"));
    cmd.current_dir(cwd)
        .env("HOME", home)
        .env_remove("BPM_CATALOG")
        .args(args);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd.output().expect("spawn bpm");
    Run {
        code: output.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&output.stdout).into_owned(),
        err: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn ok(run: Run) -> Run {
    assert_eq!(run.code, 0, "stdout:\n{}\nstderr:\n{}", run.out, run.err);
    run
}

fn fail(run: Run) -> Run {
    assert_ne!(
        run.code, 0,
        "expected failure\nstdout:\n{}\nstderr:\n{}",
        run.out, run.err
    );
    run
}

fn line(out: &str) -> String {
    out.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .last()
        .unwrap_or("")
        .to_string()
}

fn init(home: &Path, catalog: &Path) {
    ok(bpm(home, home, &["init", catalog.to_str().unwrap()]));
}

fn sql(home: &Path, catalog: &Path, statement: &str) -> String {
    ok(bpm(
        home,
        home,
        &["--catalog", catalog.to_str().unwrap(), "sql", statement],
    ))
    .out
}

fn sql_write(home: &Path, catalog: &Path, statement: &str) -> String {
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            catalog.to_str().unwrap(),
            "sql",
            "--write",
            statement,
        ],
    ))
    .out
}

fn count(home: &Path, catalog: &Path, table: &str) -> i64 {
    line(&sql(
        home,
        catalog,
        &format!("SELECT COUNT(*) AS n FROM {table}"),
    ))
    .parse()
    .unwrap()
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn bytes(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap()
}

fn mtime(path: &Path) -> SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

fn is_uuid_v7(text: impl AsRef<str>) -> bool {
    let text = text.as_ref();
    let mut parts = text.split('-');
    parts.next().is_some_and(|part| part.len() == 8)
        && parts.next().is_some_and(|part| part.len() == 4)
        && parts
            .next()
            .is_some_and(|part| part.starts_with('7') && part.len() == 4)
        && parts.next().is_some()
        && parts.next().is_some()
}

#[test]
fn scenario_01_init_modes_and_force() {
    let scratch = Scratch::new("init");
    let home = scratch.path();
    let default = home.join(".bpm").join("default.db");

    let created = ok(bpm(home, home, &["init"]));
    assert_eq!(created.out.trim(), default.display().to_string());
    assert!(default.is_file());
    #[cfg(unix)]
    {
        assert_eq!(mode(&default), 0o600);
        assert_eq!(mode(&home.join(".bpm")), 0o700);
    }

    let before = bytes(&default);
    let again = fail(bpm(home, home, &["init"]));
    assert!(
        again.err.contains("already exists") || again.err.contains(&default.display().to_string())
    );
    assert_eq!(bytes(&default), before);

    let id = line(&sql(
        home,
        &default,
        "SELECT value FROM catalog_meta WHERE key = 'catalog_id'",
    ));
    assert!(is_uuid_v7(&id), "{id}");
    let version = line(&sql(
        home,
        &default,
        "SELECT value FROM catalog_meta WHERE key = 'schema_version'",
    ));
    assert_eq!(version, "2");

    ok(bpm(home, home, &["init", "--force"]));
    let id_after = line(&sql(
        home,
        &default,
        "SELECT value FROM catalog_meta WHERE key = 'catalog_id'",
    ));
    assert_ne!(id, id_after);
    assert!(is_uuid_v7(&id_after), "{id_after}");
}

#[test]
fn scenario_02_resolution_order() {
    let scratch = Scratch::new("resolve");
    let home = scratch.path();
    let default = home.join(".bpm").join("default.db");
    let other = home.join("other.db");
    let cwd = home.join("work");
    fs::create_dir_all(&cwd).unwrap();
    let local = cwd.join("bpm.db");

    ok(bpm(home, home, &["init"]));
    ok(bpm(
        home,
        home,
        &["create", "program", "--name", "Defaulted"],
    ));
    init(home, &other);
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            other.to_str().unwrap(),
            "create",
            "program",
            "--name",
            "FromEnv",
        ],
    ));
    init(home, &local);
    ok(bpm(
        home,
        &cwd,
        &[
            "--catalog",
            local.to_str().unwrap(),
            "create",
            "program",
            "--name",
            "SittingHere",
        ],
    ));

    let bare = ok(bpm(home, &cwd, &["query", "entities"]));
    assert!(bare.out.contains("Defaulted"), "{}", bare.out);
    assert!(!bare.out.contains("FromEnv"), "{}", bare.out);
    assert!(!bare.out.contains("SittingHere"), "{}", bare.out);

    let from_env = ok(bpm_env(
        home,
        &cwd,
        &["query", "entities"],
        &[("BPM_CATALOG", other.to_str().unwrap())],
    ));
    assert!(from_env.out.contains("FromEnv"), "{}", from_env.out);
    assert!(!from_env.out.contains("Defaulted"), "{}", from_env.out);

    let from_flag = ok(bpm_env(
        home,
        &cwd,
        &["--catalog", default.to_str().unwrap(), "query", "entities"],
        &[("BPM_CATALOG", other.to_str().unwrap())],
    ));
    assert!(from_flag.out.contains("Defaulted"), "{}", from_flag.out);
    assert!(!from_flag.out.contains("FromEnv"), "{}", from_flag.out);

    let shown = ok(bpm(home, &cwd, &["catalog"]));
    assert_eq!(shown.out.trim(), default.display().to_string());
}

#[test]
fn missing_catalog_tells_the_operator_to_init() {
    let scratch = Scratch::new("missing");
    let home = scratch.path();
    let default = home.join(".bpm").join("default.db");
    let query = fail(bpm(home, home, &["query", "entities"]));
    assert!(query.err.contains("bpm init"), "{}", query.err);
    assert!(!default.exists());
    let shown = fail(bpm(home, home, &["catalog"]));
    assert!(shown.err.contains("bpm init"), "{}", shown.err);
}

#[test]
fn scenario_03_chain_and_metadata_path() {
    let scratch = Scratch::new("chain");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();

    let program = ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "program", "--name", "CLL"],
    ));
    assert_eq!(program.out.trim(), "/CLL");
    let project = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "project",
            "--parent",
            "/CLL",
            "--name",
            "WES-relapse",
        ],
    ));
    assert_eq!(project.out.trim(), "/CLL/WES-relapse");

    let case_id = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "case",
            "--parent",
            "/CLL/WES-relapse",
        ],
    ))
    .out
    .trim()
    .to_string();
    assert!(is_uuid_v7(&case_id), "{case_id}");
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "meta",
            "set",
            &case_id,
            "ext_id",
            "CLL-001",
        ],
    ));

    let sample_id = ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "sample", "--parent", &case_id],
    ))
    .out
    .trim()
    .to_string();
    assert!(is_uuid_v7(&sample_id), "{sample_id}");
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "meta",
            "set",
            &sample_id,
            "sample_kind",
            "aliquot",
        ],
    ));

    let raw_id = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "raw_data",
            "--parent",
            &sample_id,
        ],
    ))
    .out
    .trim()
    .to_string();
    let analysis_id = ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "analysis", "--parent", &raw_id],
    ))
    .out
    .trim()
    .to_string();
    assert!(is_uuid_v7(&raw_id) && is_uuid_v7(&analysis_id));

    let by_path = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "meta",
            "get",
            "/CLL/WES-relapse/ext_id:CLL-001",
            "ext_id",
        ],
    ));
    assert_eq!(by_path.out.trim(), "CLL-001");
    let chained = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "meta",
            "get",
            "/CLL/WES-relapse/ext_id:CLL-001/sample_kind:aliquot",
            "sample_kind",
        ],
    ));
    assert_eq!(chained.out.trim(), "aliquot");

    let listed = ok(bpm(home, home, &["--catalog", cat, "query", "entities"]));
    assert!(listed.out.contains("/CLL/WES-relapse"), "{}", listed.out);
    assert!(listed.out.contains(&case_id));
    assert!(listed.out.contains(&analysis_id));
}

#[test]
fn scenario_04_illegal_parent_leaves_the_catalog_unchanged() {
    let scratch = Scratch::new("parent");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "program", "--name", "CLL"],
    ));
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "project",
            "--parent",
            "/CLL",
            "--name",
            "WES",
        ],
    ));
    let cases_before = count(home, &catalog, "cases");
    let samples_before = count(home, &catalog, "samples");

    let sample = fail(bpm(
        home,
        home,
        &["--catalog", cat, "create", "sample", "--parent", "/CLL"],
    ));
    assert!(sample.err.contains("illegal parent"), "{}", sample.err);
    assert_eq!(count(home, &catalog, "samples"), samples_before);

    let second = bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "case",
            "--parent",
            "/CLL/WES",
            "--parent",
            "/CLL/WES",
        ],
    );
    assert_eq!(second.code, 2, "stderr:\n{}", second.err);
    assert_eq!(count(home, &catalog, "cases"), cases_before);

    let with_name = fail(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "program",
            "--name",
            "Other",
            "--parent",
            "/CLL",
        ],
    ));
    assert!(
        with_name.err.contains("illegal parent"),
        "{}",
        with_name.err
    );
    assert_eq!(count(home, &catalog, "programs"), 1);
}

#[test]
fn scenario_05_names() {
    let scratch = Scratch::new("names");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "program", "--name", "CLL"],
    ));
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "program", "--name", "cll"],
    ));
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "project",
            "--parent",
            "/CLL",
            "--name",
            "WES",
        ],
    ));
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "project",
            "--parent",
            "/cll",
            "--name",
            "WES",
        ],
    ));

    let dup = fail(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "project",
            "--parent",
            "/CLL",
            "--name",
            "WES",
        ],
    ));
    assert!(dup.err.contains("sibling name taken"), "{}", dup.err);
    assert_eq!(count(home, &catalog, "projects"), 2);

    for bad in ["/slash", "has:colon", " leading", "trailing ", "bad\nname"] {
        let rejected = fail(bpm(
            home,
            home,
            &["--catalog", cat, "create", "program", "--name", bad],
        ));
        assert_ne!(rejected.code, 0, "{bad}");
    }
    assert_eq!(count(home, &catalog, "programs"), 2);

    let named_case = fail(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "case",
            "--parent",
            "/CLL/WES",
            "--name",
            "Nope",
        ],
    ));
    assert!(
        named_case.err.contains("does not take a name"),
        "{}",
        named_case.err
    );
    assert_eq!(count(home, &catalog, "cases"), 0);
}

#[test]
fn scenario_06_rename_and_reparent() {
    let scratch = Scratch::new("move");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "program", "--name", "CLL"],
    ));
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "project",
            "--parent",
            "/CLL",
            "--name",
            "WES-relapse",
        ],
    ));
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "project",
            "--parent",
            "/CLL",
            "--name",
            "Other",
        ],
    ));
    let case_id = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "case",
            "--parent",
            "/CLL/WES-relapse",
        ],
    ))
    .out
    .trim()
    .to_string();
    let sample_id = ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "sample", "--parent", &case_id],
    ))
    .out
    .trim()
    .to_string();

    ok(bpm(
        home,
        home,
        &["--catalog", cat, "rename", "/CLL", "CLL2"],
    ));
    let after_rename = ok(bpm(
        home,
        home,
        &["--catalog", cat, "query", "entities", "--type", "project"],
    ));
    assert!(
        after_rename.out.contains("/CLL2/WES-relapse"),
        "{}",
        after_rename.out
    );
    assert!(
        !after_rename.out.contains("/CLL/WES-relapse"),
        "{}",
        after_rename.out
    );
    let still = ok(bpm(
        home,
        home,
        &["--catalog", cat, "meta", "get", &case_id],
    ));
    assert_eq!(still.code, 0);
    assert!(still.out.is_empty() || !still.out.contains("error"));

    ok(bpm(
        home,
        home,
        &["--catalog", cat, "reparent", &case_id, "/CLL2/Other"],
    ));
    let under_other = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "query",
            "entities",
            "--under",
            "/CLL2/Other",
        ],
    ));
    assert!(under_other.out.contains(&case_id), "{}", under_other.out);
    let under_wes = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "query",
            "entities",
            "--under",
            "/CLL2/WES-relapse",
        ],
    ));
    assert!(!under_wes.out.contains(&case_id), "{}", under_wes.out);

    let onto_sample = fail(bpm(
        home,
        home,
        &["--catalog", cat, "reparent", &case_id, &sample_id],
    ));
    assert!(
        onto_sample.err.contains("illegal parent"),
        "{}",
        onto_sample.err
    );
    let still_there = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "query",
            "entities",
            "--under",
            "/CLL2/Other",
        ],
    ));
    assert!(still_there.out.contains(&case_id), "{}", still_there.out);
}

#[test]
fn scenario_07_metadata_selectors() {
    let scratch = Scratch::new("meta");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "program", "--name", "CLL"],
    ));
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "project",
            "--parent",
            "/CLL",
            "--name",
            "WES-relapse",
        ],
    ));
    let case_id = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "case",
            "--parent",
            "/CLL/WES-relapse",
        ],
    ))
    .out
    .trim()
    .to_string();
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "meta",
            "set",
            &case_id,
            "subject_id",
            "CLL-001",
        ],
    ));
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "meta",
            "set",
            &case_id,
            "subject_id",
            "CLL-002",
        ],
    ));
    assert_eq!(
        ok(bpm(
            home,
            home,
            &["--catalog", cat, "meta", "get", &case_id, "subject_id"]
        ))
        .out
        .trim(),
        "CLL-002"
    );
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "meta",
            "set",
            &case_id,
            "subject_id",
            "CLL-001",
        ],
    ));

    let table = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "query",
            "entities",
            "--where",
            "subject_id:CLL-001",
        ],
    ));
    assert!(table.out.contains("subject_id=CLL-001"), "{}", table.out);
    let by_key = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "query",
            "entities",
            "--where",
            "subject_id:",
        ],
    ));
    assert!(by_key.out.contains(&case_id), "{}", by_key.out);
    let by_value = ok(bpm(
        home,
        home,
        &["--catalog", cat, "query", "entities", "--where", ":CLL-001"],
    ));
    assert!(by_value.out.contains(&case_id), "{}", by_value.out);

    let json = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "query",
            "entities",
            "--where",
            "subject_id:CLL-001",
            "--format",
            "json",
        ],
    ));
    let parsed: serde_json::Value = serde_json::from_str(json.out.trim()).unwrap();
    assert_eq!(parsed[0]["metadata"]["subject_id"], "CLL-001");

    let csv = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "query",
            "entities",
            "--where",
            "subject_id:CLL-001",
            "--format",
            "csv",
        ],
    ));
    assert!(csv.out.lines().next().unwrap().contains("metadata"));
    assert!(csv.out.contains("subject_id=CLL-001"));

    for bad in ["bad:value", "bad/value"] {
        let rejected = fail(bpm(
            home,
            home,
            &["--catalog", cat, "meta", "set", &case_id, "note", bad],
        ));
        assert!(rejected.err.contains("metadata"), "{}", rejected.err);
    }
    let missing = fail(bpm(
        home,
        home,
        &["--catalog", cat, "meta", "get", &case_id, "note"],
    ));
    assert!(missing.err.contains("not set"), "{}", missing.err);

    let other = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "case",
            "--parent",
            "/CLL/WES-relapse",
        ],
    ))
    .out
    .trim()
    .to_string();
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "meta",
            "set",
            &other,
            "subject_id",
            "CLL-001",
        ],
    ));
    let ambiguous = fail(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "meta",
            "get",
            "/CLL/WES-relapse/subject_id:CLL-001",
            "subject_id",
        ],
    ));
    assert!(ambiguous.err.contains("more than one"), "{}", ambiguous.err);
    let both = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "query",
            "entities",
            "--where",
            "subject_id:CLL-001",
        ],
    ));
    assert!(
        both.out.contains(&case_id) && both.out.contains(&other),
        "{}",
        both.out
    );

    ok(bpm(
        home,
        home,
        &["--catalog", cat, "meta", "unset", &other, "subject_id"],
    ));
    let unset = fail(bpm(
        home,
        home,
        &["--catalog", cat, "meta", "get", &other, "subject_id"],
    ));
    assert!(unset.err.contains("not set"), "{}", unset.err);
    let again = fail(bpm(
        home,
        home,
        &["--catalog", cat, "meta", "unset", &other, "subject_id"],
    ));
    assert!(again.err.contains("not set"), "{}", again.err);
}

#[test]
fn scenario_08_and_25_conventional_keys_are_data() {
    let scratch = Scratch::new("keys");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "program", "--name", "CLL"],
    ));
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "project",
            "--parent",
            "/CLL",
            "--name",
            "WES",
        ],
    ));
    let case_id = ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "case", "--parent", "/CLL/WES"],
    ))
    .out
    .trim()
    .to_string();
    let sample_id = ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "sample", "--parent", &case_id],
    ))
    .out
    .trim()
    .to_string();
    let raw_id = ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "raw_data",
            "--parent",
            &sample_id,
        ],
    ))
    .out
    .trim()
    .to_string();

    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "meta",
            "set",
            &sample_id,
            "sample_kind",
            "not-a-kind",
        ],
    ));
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "meta", "set", &raw_id, "assay", "WES"],
    ));
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "meta", "set", &case_id, "consent", "GRU"],
    ));
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "meta",
            "set",
            &case_id,
            "embargo_until",
            "2099-01-01",
        ],
    ));

    assert_eq!(
        ok(bpm(
            home,
            home,
            &["--catalog", cat, "meta", "get", &sample_id, "sample_kind"]
        ))
        .out
        .trim(),
        "not-a-kind"
    );
    for (selector, id) in [
        ("sample_kind:not-a-kind", sample_id.as_str()),
        ("assay:WES", raw_id.as_str()),
        ("consent:GRU", case_id.as_str()),
        ("embargo_until:2099-01-01", case_id.as_str()),
    ] {
        let found = ok(bpm(
            home,
            home,
            &["--catalog", cat, "query", "entities", "--where", selector],
        ));
        assert!(found.out.contains(id), "{selector}\n{}", found.out);
    }
    let all = ok(bpm(home, home, &["--catalog", cat, "query", "entities"]));
    assert!(all.out.contains("embargo_until=2099-01-01"), "{}", all.out);
}

#[test]
fn scenario_09_metadata_is_not_copied_onto_children() {
    let scratch = Scratch::new("iso-meta");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "program", "--name", "CLL"],
    ));
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "project",
            "--parent",
            "/CLL",
            "--name",
            "WES",
        ],
    ));
    let case_id = ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "case", "--parent", "/CLL/WES"],
    ))
    .out
    .trim()
    .to_string();
    let sample_id = ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "sample", "--parent", &case_id],
    ))
    .out
    .trim()
    .to_string();
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "meta", "set", &case_id, "consent", "GRU"],
    ));

    let missing = fail(bpm(
        home,
        home,
        &["--catalog", cat, "meta", "get", &sample_id, "consent"],
    ));
    assert!(missing.err.contains("not set"), "{}", missing.err);

    let under = ok(bpm(
        home,
        home,
        &["--catalog", cat, "query", "entities", "--under", &case_id],
    ));
    let case_line = under
        .out
        .lines()
        .find(|line| line.contains(&case_id))
        .expect(&under.out);
    let sample_line = under
        .out
        .lines()
        .find(|line| line.contains(&sample_id))
        .expect(&under.out);
    assert!(case_line.contains("consent=GRU"), "{case_line}");
    assert!(!sample_line.contains("consent"), "{sample_line}");
}

#[test]
fn scenario_27_sql_hatch_does_not_record_history() {
    let scratch = Scratch::new("sql");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "program", "--name", "CLL"],
    ));

    let selected = sql(home, &catalog, "SELECT name FROM programs");
    assert!(selected.contains("CLL"), "{selected}");

    let denied = fail(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "sql",
            "UPDATE programs SET name = 'nope' WHERE name = 'CLL'",
        ],
    ));
    assert!(!denied.err.is_empty(), "{}", denied.err);
    let unchanged = sql(home, &catalog, "SELECT name FROM programs");
    assert!(unchanged.contains("CLL"), "{unchanged}");
    assert!(!unchanged.contains("nope"), "{unchanged}");

    sql_write(
        home,
        &catalog,
        "UPDATE programs SET name = 'CLL2' WHERE name = 'CLL'",
    );
    let changed = sql(home, &catalog, "SELECT name FROM programs");
    assert!(changed.contains("CLL2"), "{changed}");

    let history = sql(
        home,
        &catalog,
        "SELECT name FROM sqlite_schema WHERE name = 'command_history'",
    );
    assert!(!history.contains("command_history"), "{history}");
}

#[test]
fn scenario_30_two_catalogs_do_not_share_rows() {
    let scratch = Scratch::new("two");
    let home = scratch.path();
    let a = home.join("a.db");
    let b = home.join("b.db");
    init(home, &a);
    init(home, &b);
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            a.to_str().unwrap(),
            "create",
            "program",
            "--name",
            "OnlyA",
        ],
    ));
    let from_b = ok(bpm(
        home,
        home,
        &["--catalog", b.to_str().unwrap(), "query", "entities"],
    ));
    assert!(!from_b.out.contains("OnlyA"), "{}", from_b.out);
    let from_a = ok(bpm(
        home,
        home,
        &["--catalog", a.to_str().unwrap(), "query", "entities"],
    ));
    assert!(from_a.out.contains("OnlyA"), "{}", from_a.out);
    let id_a = line(&sql(
        home,
        &a,
        "SELECT value FROM catalog_meta WHERE key = 'catalog_id'",
    ));
    let id_b = line(&sql(
        home,
        &b,
        "SELECT value FROM catalog_meta WHERE key = 'catalog_id'",
    ));
    assert_ne!(id_a, id_b);
}

#[test]
fn delete_refuses_children_and_cascade_keeps_file_rows() {
    let scratch = Scratch::new("delete");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "program", "--name", "CLL"],
    ));
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "project",
            "--parent",
            "/CLL",
            "--name",
            "WES",
        ],
    ));
    let case_id = ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "case", "--parent", "/CLL/WES"],
    ))
    .out
    .trim()
    .to_string();
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "meta",
            "set",
            &case_id,
            "subject_id",
            "CLL-001",
        ],
    ));

    let blocked = fail(bpm(home, home, &["--catalog", cat, "delete", "/CLL"]));
    assert!(
        blocked.err.contains("children") || blocked.err.contains("linked"),
        "{}",
        blocked.err
    );
    assert_eq!(count(home, &catalog, "programs"), 1);
    assert_eq!(count(home, &catalog, "projects"), 1);

    let program_id = line(&sql(home, &catalog, "SELECT id FROM programs"));
    let file_id = "11111111-1111-7111-8111-111111111111";
    sql_write(
        home,
        &catalog,
        &format!("INSERT INTO files (id, created_at) VALUES ('{file_id}', CURRENT_TIMESTAMP)"),
    );
    sql_write(
        home,
        &catalog,
        &format!(
            "INSERT INTO file_links (file_id, node_type, node_id, role) VALUES ('{file_id}', 'program', '{program_id}', 'data')"
        ),
    );
    let linked = fail(bpm(home, home, &["--catalog", cat, "delete", "/CLL/WES"]));
    assert_ne!(linked.code, 0);
    // The project has a child, so it is refused before the program's link matters.
    let leaf_blocked = fail(bpm(home, home, &["--catalog", cat, "delete", "/CLL"]));
    assert_ne!(leaf_blocked.code, 0);

    ok(bpm(
        home,
        home,
        &["--catalog", cat, "delete", "--cascade", "/CLL"],
    ));
    assert_eq!(count(home, &catalog, "programs"), 0);
    assert_eq!(count(home, &catalog, "projects"), 0);
    assert_eq!(count(home, &catalog, "cases"), 0);
    assert_eq!(count(home, &catalog, "entity_metadata"), 0);
    assert_eq!(count(home, &catalog, "file_links"), 0);
    assert_eq!(count(home, &catalog, "files"), 1);
}

#[test]
fn schema_newer_is_refused_and_a_missing_version_is_corrupt() {
    let scratch = Scratch::new("schema");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    sql_write(
        home,
        &catalog,
        "UPDATE catalog_meta SET value = '99' WHERE key = 'schema_version'",
    );
    let before = bytes(&catalog);
    let stamp = mtime(&catalog);
    std::thread::sleep(Duration::from_millis(20));
    let refused = fail(bpm(home, home, &["--catalog", cat, "query", "entities"]));
    assert!(
        refused.err.contains("99") && refused.err.contains("(version 2)"),
        "{}",
        refused.err
    );
    assert_eq!(bytes(&catalog), before);
    assert_eq!(mtime(&catalog), stamp);

    // A second, fresh catalog with the version key deleted is corrupt.
    let fresh = home.join("fresh.db");
    init(home, &fresh);
    sql_write(
        home,
        &fresh,
        "DELETE FROM catalog_meta WHERE key = 'schema_version'",
    );
    let corrupt = fail(bpm(
        home,
        home,
        &["--catalog", fresh.to_str().unwrap(), "query", "entities"],
    ));
    assert!(corrupt.err.contains("schema_version"), "{}", corrupt.err);
    let still = fail(bpm(
        home,
        home,
        &["--catalog", fresh.to_str().unwrap(), "sql", "SELECT 1"],
    ));
    assert!(still.err.contains("schema_version"), "{}", still.err);
}

#[test]
fn catalog_lock_rejects_a_second_writer() {
    let scratch = Scratch::new("lock");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "program", "--name", "CLL"],
    ));

    // Another process holds SQLite's write lock for the whole command.
    let mut other = Connection::open(&catalog).unwrap();
    let held = other
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();

    let busy = fail(bpm(
        home,
        home,
        &["--catalog", cat, "rename", "/CLL", "CLL2"],
    ));
    assert!(busy.err.contains("catalog is busy"), "{}", busy.err);
    assert!(busy.err.contains(cat), "{}", busy.err);
    held.rollback().unwrap();
    let name = sql(home, &catalog, "SELECT name FROM programs");
    assert!(name.contains("CLL") && !name.contains("CLL2"), "{name}");
}

#[test]
fn readers_see_committed_rows_while_a_writer_holds_the_lock() {
    let scratch = Scratch::new("wal");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "program", "--name", "CLL"],
    ));

    let mut other = Connection::open(&catalog).unwrap();
    let held = other
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    held.execute(
        "UPDATE programs SET name = 'uncommitted' WHERE name = 'CLL'",
        [],
    )
    .unwrap();

    // Both the SQL hatch and a query read while the write is open, and neither
    // sees the uncommitted change.
    let name = sql(home, &catalog, "SELECT name FROM programs");
    assert!(
        name.contains("CLL") && !name.contains("uncommitted"),
        "{name}"
    );
    let listed = ok(bpm(home, home, &["--catalog", cat, "query", "entities"]));
    assert!(listed.out.contains("/CLL"), "{}", listed.out);

    held.commit().unwrap();
    let name = sql(home, &catalog, "SELECT name FROM programs");
    assert!(name.contains("uncommitted"), "{name}");
}

#[test]
fn sql_write_cannot_orphan_a_parent_or_a_file_link() {
    let scratch = Scratch::new("fk");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "program", "--name", "CLL"],
    ));
    ok(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "create",
            "project",
            "--parent",
            "/CLL",
            "--name",
            "WES",
        ],
    ));
    let program_id = line(&sql(home, &catalog, "SELECT id FROM programs"));

    let orphan = fail(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "sql",
            "--write",
            "INSERT INTO projects (id, program_id, name, created_at, updated_at) \
             VALUES ('11111111-1111-7111-8111-111111111111', 'missing', 'P', '', '')",
        ],
    ));
    assert!(orphan.err.contains("FOREIGN KEY"), "{}", orphan.err);
    let link = fail(bpm(
        home,
        home,
        &[
            "--catalog",
            cat,
            "sql",
            "--write",
            &format!(
                "INSERT INTO file_links (file_id, node_type, node_id, role) \
                 VALUES ('no-such-file', 'program', '{program_id}', 'data')"
            ),
        ],
    ));
    assert!(link.err.contains("FOREIGN KEY"), "{}", link.err);
    let parent_delete = fail(bpm(
        home,
        home,
        &["--catalog", cat, "sql", "--write", "DELETE FROM programs"],
    ));
    assert!(
        parent_delete.err.contains("FOREIGN KEY"),
        "{}",
        parent_delete.err
    );
    assert_eq!(count(home, &catalog, "programs"), 1);
    assert_eq!(count(home, &catalog, "projects"), 1);
    assert_eq!(count(home, &catalog, "file_links"), 0);
}

#[test]
fn a_sqlite_file_that_bpm_did_not_create_is_refused() {
    let scratch = Scratch::new("foreign");
    let home = scratch.path();
    let foreign = home.join("other.db");
    {
        let conn = Connection::open(&foreign).unwrap();
        conn.execute("CREATE TABLE notes (body TEXT)", []).unwrap();
    }
    let before = bytes(&foreign);
    let path = foreign.to_str().unwrap();
    for args in [
        vec!["--catalog", path, "create", "program", "--name", "CLL"],
        vec!["--catalog", path, "query", "entities"],
    ] {
        let refused = fail(bpm(home, home, &args));
        assert!(refused.err.contains("not a BPM catalog"), "{}", refused.err);
    }
    assert_eq!(bytes(&foreign), before);
}

#[test]
fn later_milestones_are_not_stubbed_as_success() {
    let scratch = Scratch::new("later");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    let impact = fail(bpm(
        home,
        home,
        &["--catalog", cat, "query", "impact", "--under", "/CLL"],
    ));
    assert!(
        impact.err.contains("not part of this milestone"),
        "{}",
        impact.err
    );
    let login = fail(bpm(home, home, &["login"]));
    assert!(
        login.err.contains("no server is configured"),
        "{}",
        login.err
    );
}

// Regression tests for the review of the SQLite switch.

/// Open a catalog the way a third-party SQLite shell might: foreign keys off.
fn raw(catalog: &Path) -> Connection {
    let conn = Connection::open(catalog).unwrap();
    conn.pragma_update(None, "foreign_keys", false).unwrap();
    conn
}

#[test]
fn overlapping_inits_leave_exactly_one_catalog() {
    let scratch = Scratch::new("init-race");
    let home = scratch.path();
    for round in 0..10 {
        let catalog = home.join(format!("race-{round}.db"));
        let cat = catalog.to_str().unwrap().to_string();
        let children: Vec<_> = (0..4)
            .map(|_| {
                Command::new(env!("CARGO_BIN_EXE_bpm"))
                    .current_dir(home)
                    .env("HOME", home)
                    .env_remove("BPM_CATALOG")
                    .args(["init", &cat])
                    .output()
                    .expect("spawn bpm")
            })
            .collect();
        let winners = children.iter().filter(|out| out.status.success()).count();
        assert_eq!(winners, 1, "round {round}: {winners} inits succeeded");
        for out in children.iter().filter(|out| !out.status.success()) {
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(err.contains("already exists"), "round {round}: {err}");
        }
        assert!(catalog.is_file(), "round {round}: catalog is gone");
        let id = line(&sql(
            home,
            &catalog,
            "SELECT value FROM catalog_meta WHERE key = 'catalog_id'",
        ));
        assert!(is_uuid_v7(&id), "round {round}: {id}");
    }
}

#[test]
fn init_force_refuses_a_busy_catalog_and_leaves_it_intact() {
    let scratch = Scratch::new("force-busy");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    let id_query = "SELECT value FROM catalog_meta WHERE key = 'catalog_id'";
    let id = line(&sql(home, &catalog, id_query));

    let mut other = Connection::open(&catalog).unwrap();
    let held = other
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let busy = fail(bpm(home, home, &["init", "--force", cat]));
    assert!(busy.err.contains("catalog is busy"), "{}", busy.err);
    held.rollback().unwrap();
    drop(other);
    assert_eq!(line(&sql(home, &catalog, id_query)), id);

    ok(bpm(home, home, &["init", "--force", cat]));
    let replaced = line(&sql(home, &catalog, id_query));
    assert_ne!(replaced, id);
    assert!(is_uuid_v7(&replaced), "{replaced}");
    assert_eq!(line(&sql(home, &catalog, "PRAGMA journal_mode")), "wal");
    assert_eq!(count(home, &catalog, "programs"), 0);
    #[cfg(unix)]
    assert_eq!(mode(&catalog), 0o600);
}

#[test]
fn init_force_replaces_a_file_that_is_not_a_database() {
    let scratch = Scratch::new("force-foreign");
    let home = scratch.path();
    let catalog = home.join("notes.db");
    fs::write(&catalog, "not a database\n").unwrap();
    let cat = catalog.to_str().unwrap();

    let refused = fail(bpm(home, home, &["init", cat]));
    assert!(refused.err.contains("already exists"), "{}", refused.err);
    ok(bpm(home, home, &["init", "--force", cat]));
    let id = line(&sql(
        home,
        &catalog,
        "SELECT value FROM catalog_meta WHERE key = 'catalog_id'",
    ));
    assert!(is_uuid_v7(&id), "{id}");
    let leftovers: Vec<_> = fs::read_dir(home)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".init-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn tables_without_a_valid_version_are_corrupt_for_readers_and_writers() {
    let scratch = Scratch::new("version");
    let home = scratch.path();

    // A file with BPM's application id and catalog_meta, but no version row.
    let meta_only = home.join("meta-only.db");
    {
        let conn = Connection::open(&meta_only).unwrap();
        conn.pragma_update(None, "application_id", 0x4250_4D43)
            .unwrap();
        conn.execute(
            "CREATE TABLE catalog_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
            [],
        )
        .unwrap();
    }
    // A real catalog whose version was set to -1.
    let negative = home.join("negative.db");
    init(home, &negative);
    raw(&negative)
        .execute(
            "UPDATE catalog_meta SET value = '-1' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();

    for catalog in [&meta_only, &negative] {
        let cat = catalog.to_str().unwrap();
        for args in [
            vec!["--catalog", cat, "query", "entities"],
            vec!["--catalog", cat, "create", "program", "--name", "CLL"],
        ] {
            let refused = fail(bpm(home, home, &args));
            assert!(
                refused.err.contains("no valid schema_version"),
                "{args:?}: {}",
                refused.err
            );
            assert!(!refused.err.contains("CREATE TABLE"), "{}", refused.err);
        }
    }
}

#[test]
fn an_orphan_entity_is_reported_and_repaired_instead_of_panicking() {
    let scratch = Scratch::new("orphan");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "program", "--name", "CLL"],
    ));

    // A project whose program does not exist, with a case, metadata, and a link
    // under it, written with foreign keys off.
    let project = "11111111-1111-7111-8111-111111111111";
    let case = "22222222-2222-7222-8222-222222222222";
    let file = "33333333-3333-7333-8333-333333333333";
    raw(&catalog)
        .execute_batch(&format!(
            "INSERT INTO projects VALUES ('{project}', 'no-such-program', 'Lost', 't', 't');
             INSERT INTO cases VALUES ('{case}', '{project}', 't', 't');
             INSERT INTO entity_metadata VALUES ('case', '{case}', 'subject_id', 'X', 't');
             INSERT INTO files (id, created_at) VALUES ('{file}', 't');
             INSERT INTO file_links VALUES ('{file}', 'case', '{case}', 'data');
             INSERT INTO entity_metadata VALUES ('sample', 'no-such-sample', 'k', 'v', 't');"
        ))
        .unwrap();

    let refused = bpm(home, home, &["--catalog", cat, "query", "entities"]);
    assert_eq!(refused.code, 1, "{}", refused.err);
    assert!(refused.err.contains("inconsistent"), "{}", refused.err);
    assert!(refused.err.contains("bpm repair"), "{}", refused.err);

    let report = fail(bpm(home, home, &["--catalog", cat, "repair"]));
    assert!(report.out.contains(project), "{}", report.out);
    assert!(report.out.contains("no-such-sample"), "{}", report.out);
    assert!(report.err.contains("repair --apply"), "{}", report.err);
    assert_eq!(count(home, &catalog, "projects"), 1);

    let repaired = ok(bpm(home, home, &["--catalog", cat, "repair", "--apply"]));
    assert!(repaired.out.contains("1 descendant"), "{}", repaired.out);
    assert_eq!(count(home, &catalog, "projects"), 0);
    assert_eq!(count(home, &catalog, "cases"), 0);
    assert_eq!(count(home, &catalog, "entity_metadata"), 0);
    assert_eq!(count(home, &catalog, "file_links"), 0);
    assert_eq!(count(home, &catalog, "files"), 1);
    assert_eq!(count(home, &catalog, "programs"), 1);

    let clean = ok(bpm(home, home, &["--catalog", cat, "repair"]));
    assert!(clean.out.contains("consistent"), "{}", clean.out);
    let listed = ok(bpm(home, home, &["--catalog", cat, "query", "entities"]));
    assert!(listed.out.contains("/CLL"), "{}", listed.out);
}

#[test]
fn an_orphan_can_be_reparented_instead_of_removed() {
    let scratch = Scratch::new("orphan-reparent");
    let home = scratch.path();
    let catalog = home.join("cat.db");
    init(home, &catalog);
    let cat = catalog.to_str().unwrap();
    ok(bpm(
        home,
        home,
        &["--catalog", cat, "create", "program", "--name", "CLL"],
    ));
    let project = "11111111-1111-7111-8111-111111111111";
    raw(&catalog)
        .execute(
            &format!(
                "INSERT INTO projects VALUES ('{project}', '44444444-4444-7444-8444-444444444444', 'Lost', 't', 't')"
            ),
            [],
        )
        .unwrap();

    ok(bpm(
        home,
        home,
        &["--catalog", cat, "reparent", project, "/CLL"],
    ));
    let listed = ok(bpm(home, home, &["--catalog", cat, "query", "entities"]));
    assert!(listed.out.contains("/CLL/Lost"), "{}", listed.out);
    ok(bpm(home, home, &["--catalog", cat, "repair"]));
}

#[cfg(unix)]
#[test]
fn a_shared_bpm_directory_is_warned_about_but_not_refused() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new("bpm-mode");
    let home = scratch.path();
    let dir = home.join(".bpm");
    fs::create_dir(&dir).unwrap();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

    let created = ok(bpm(home, home, &["init"]));
    assert!(created.err.contains("warning"), "{}", created.err);
    assert!(created.err.contains("chmod 700"), "{}", created.err);
    let listed = ok(bpm(home, home, &["query", "entities"]));
    assert!(listed.err.contains("warning"), "{}", listed.err);

    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    let quiet = ok(bpm(home, home, &["query", "entities"]));
    assert!(!quiet.err.contains("warning"), "{}", quiet.err);
}
