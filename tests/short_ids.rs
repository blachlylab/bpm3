//! Short ids: every command that takes an id takes its last twelve hex
//! digits, a short id two rows share is refused, and tables show an id in full
//! when its short id is shared. Each test uses its own HOME, catalog, and data
//! directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "bpm3-short-{label}-{}-{}",
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

/// A HOME, a catalog in it, a data directory, and `/P/J`.
struct Env {
    scratch: Scratch,
    catalog: PathBuf,
}

impl Env {
    fn new(label: &str) -> Self {
        let scratch = Scratch::new(label);
        let catalog = scratch.0.join("cat.db");
        let env = Self { scratch, catalog };
        fs::create_dir_all(env.data()).unwrap();
        ok(env.raw(&["init", env.catalog.to_str().unwrap()]));
        ok(env.bpm(&["create", "program", "--name", "P"]));
        ok(env.bpm(&["create", "project", "--parent", "/P", "--name", "J"]));
        env
    }

    fn data(&self) -> PathBuf {
        self.scratch.0.join("data")
    }

    fn raw(&self, args: &[&str]) -> Run {
        let output = Command::new(env!("CARGO_BIN_EXE_bpm"))
            .current_dir(&self.scratch.0)
            .env("HOME", &self.scratch.0)
            .env_remove("BPM_CATALOG")
            .args(args)
            .output()
            .expect("spawn bpm");
        Run {
            code: output.status.code().unwrap_or(-1),
            out: String::from_utf8_lossy(&output.stdout).into_owned(),
            err: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    fn bpm(&self, args: &[&str]) -> Run {
        let mut full = vec!["--catalog", self.catalog.to_str().unwrap()];
        full.extend_from_slice(args);
        self.raw(&full)
    }

    /// Write `bytes` at each relative path under the data directory.
    fn write(&self, names: &[&str], bytes: &str) {
        for name in names {
            let path = self.data().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
        }
    }

    fn track(&self, dir: &Path) {
        ok(self.bpm(&["track", dir.to_str().unwrap()]));
    }

    fn scalar(&self, statement: &str) -> String {
        let out = ok(self.bpm(&["sql", statement])).out;
        out.lines().nth(1).unwrap_or("").trim().to_string()
    }

    fn count(&self, table: &str) -> i64 {
        self.scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .parse()
            .unwrap()
    }

    fn id_at(&self, path: &Path) -> String {
        self.scalar(&format!(
            "SELECT file_id FROM file_locations WHERE uri = '{}'",
            path.display()
        ))
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

fn short(id: &str) -> &str {
    &id[id.len() - 12..]
}

fn created(run: Run) -> String {
    ok(run).out.trim().to_string()
}

#[test]
fn every_command_that_takes_an_id_takes_its_short_id() {
    let env = Env::new("inputs");
    let case = created(env.bpm(&["create", "case", "--parent", "/P/J"]));
    ok(env.bpm(&["meta", "set", short(&case), "subject_id", "S1"]));
    let got = ok(env.bpm(&["meta", "get", short(&case), "subject_id"]));
    assert_eq!(got.out.trim(), "S1");
    // Either case, and with selectors after it.
    let upper = short(&case).to_ascii_uppercase();
    let sample = created(env.bpm(&["create", "sample", "--parent", &upper]));
    ok(env.bpm(&["meta", "set", short(&sample), "sample_kind", "tumor"]));
    let selected = format!("{}/sample_kind:tumor", short(&case));
    assert_eq!(
        ok(env.bpm(&["meta", "get", &selected, "sample_kind"]))
            .out
            .trim(),
        "tumor"
    );
    let under = ok(env.bpm(&["query", "entities", "--under", short(&case)]));
    assert!(under.out.contains(short(&sample)), "{}", under.out);

    env.write(&["run42/a.fq"], "a\n");
    env.track(&env.data().join("run42"));
    let file = env.id_at(&env.data().join("run42/a.fq"));
    let linked = ok(env.bpm(&[
        "link",
        "--yes",
        "--to",
        short(&sample),
        "--role",
        "data",
        short(&file),
    ]));
    let run = linked
        .out
        .lines()
        .find_map(|line| line.strip_prefix("link run "))
        .and_then(|rest| rest.split(':').next())
        .expect(&linked.out)
        .to_string();
    let files = ok(env.bpm(&["query", "files", "--under", short(&case)]));
    assert!(files.out.contains(short(&file)), "{}", files.out);
    ok(env.bpm(&["acknowledge", short(&file)]));
    ok(env.bpm(&["undo", short(&run), "--yes"]));
    assert_eq!(env.count("file_links"), 0);
    ok(env.bpm(&[
        "link",
        "--yes",
        "--to",
        short(&sample),
        "--role",
        "data",
        &file,
    ]));
    ok(env.bpm(&["unlink", "--to", short(&sample), short(&file)]));
    assert_eq!(env.count("file_links"), 0);

    let project = env.scalar("SELECT id FROM projects");
    ok(env.bpm(&["rename", short(&project), "K"]));
    // A program or project prints its path, so its id comes from the catalog.
    ok(env.bpm(&["create", "project", "--parent", "/P", "--name", "L"]));
    let other = env.scalar("SELECT id FROM projects WHERE name = 'L'");
    ok(env.bpm(&["reparent", short(&case), short(&other)]));
    assert_eq!(
        env.scalar(&format!("SELECT project_id FROM cases WHERE id = '{case}'")),
        other
    );
    ok(env.bpm(&["delete", "--cascade", short(&case)]));
    assert_eq!(env.count("cases") + env.count("samples"), 0);
}

#[test]
fn a_short_id_two_rows_share_is_refused_and_shown_in_full() {
    let env = Env::new("shared");
    env.write(&["run42/a.fq"], "a\n");
    env.track(&env.data().join("run42"));
    let file = env.id_at(&env.data().join("run42/a.fq"));
    let twin = format!("00000000-0000-7000-8000-{}", short(&file));
    ok(env.bpm(&[
        "sql",
        "--write",
        &format!(
            "INSERT INTO files (id, created_at) VALUES ('{twin}', '2026-10-27T00:00:00.000Z')"
        ),
    ]));
    let refused = fail(env.bpm(&["acknowledge", short(&file)]));
    assert!(
        refused.err.contains(&file) && refused.err.contains(&twin),
        "{}",
        refused.err
    );
    assert!(refused.err.contains("use the full id"), "{}", refused.err);
    ok(env.bpm(&["acknowledge", &file]));
    // Both rows are shown in full, so what the table shows resolves.
    let table = ok(env.bpm(&["query", "files"]));
    assert!(table.out.contains(&file), "{}", table.out);

    let program = env.scalar("SELECT id FROM programs");
    let other = format!("00000000-0000-7000-8000-{}", short(&program));
    ok(env.bpm(&[
        "sql",
        "--write",
        &format!(
            "INSERT INTO programs (id, name, created_at, updated_at)
             VALUES ('{other}', 'Q', '2026-10-27T00:00:00.000Z', '2026-10-27T00:00:00.000Z')"
        ),
    ]));
    let refused = fail(env.bpm(&["meta", "set", short(&program), "k", "v"]));
    assert!(
        refused.err.contains(&program) && refused.err.contains(&other),
        "{}",
        refused.err
    );
    let entities = ok(env.bpm(&["query", "entities", "--type", "program"]));
    assert!(
        entities.out.contains(&program) && entities.out.contains(&other),
        "{}",
        entities.out
    );
    // The project's parent column names the program by path, not by id.
    let projects = ok(env.bpm(&["query", "entities", "--type", "project"]));
    assert!(projects.out.contains("  /P"), "{}", projects.out);
}

#[test]
fn a_file_named_like_a_short_id_is_written_with_dot_slash() {
    let env = Env::new("dotslash");
    fs::write(env.scratch.0.join("abcdefabcdef"), "bytes\n").unwrap();
    ok(env.bpm(&[
        "track",
        "--whitelist",
        "abcdefabcdef",
        env.scratch.0.to_str().unwrap(),
    ]));
    let as_id = fail(env.bpm(&[
        "link",
        "--yes",
        "--to",
        "/P/J",
        "--role",
        "data",
        "abcdefabcdef",
    ]));
    assert!(as_id.err.contains("abcdefabcdef"), "{}", as_id.err);
    ok(env.bpm(&[
        "link",
        "--yes",
        "--to",
        "/P/J",
        "--role",
        "data",
        "./abcdefabcdef",
    ]));
    assert_eq!(env.count("file_links"), 1);
}

#[test]
fn a_short_id_is_twelve_digits_exactly() {
    let env = Env::new("length");
    let case = created(env.bpm(&["create", "case", "--parent", "/P/J"]));
    let tail = short(&case);
    for wrong in [&tail[1..], &case[case.len() - 13..]] {
        let refused = fail(env.bpm(&["meta", "set", wrong, "k", "v"]));
        assert!(!refused.err.is_empty(), "{wrong}");
    }
    assert_eq!(env.count("entity_metadata"), 0);
}
