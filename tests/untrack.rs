//! Untrack: scenario 40 and the rules in the untrack guide. Each test uses its
//! own HOME, catalog, and data directory. A test process has no terminal, so
//! an untrack that removes a file row needs `--yes`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "bpm3-untrack-{label}-{}-{}",
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

    fn untrack(&self, args: &[&str]) -> Run {
        let mut full = vec!["untrack"];
        full.extend_from_slice(args);
        self.bpm(&full)
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

fn s(path: &Path) -> &str {
    path.to_str().unwrap()
}

#[test]
fn scenario_40_a_file_with_another_copy_keeps_its_id() {
    let env = Env::new("copies");
    env.write(&["run42/S1.bam", "archive/S1.bam"], "reads\n");
    env.track(&env.data());
    let run42 = env.data().join("run42");
    let id = env.id_at(&run42.join("S1.bam"));
    assert_eq!(env.id_at(&env.data().join("archive/S1.bam")), id);
    // `bpm meta` takes entities; a file's metadata is written directly.
    ok(env.bpm(&[
        "sql",
        "--write",
        &format!(
            "INSERT INTO file_metadata (file_id, key, value, updated_at)
             VALUES ('{id}', 'lane', '3', '2026-10-27T00:00:00.000Z')"
        ),
    ]));

    // One copy goes without a prompt; the file keeps its id and metadata.
    let run = ok(env.untrack(&[s(&run42)]));
    assert!(
        run.out
            .contains("remove 1 locations from files with other copies, 0 files"),
        "{}",
        run.out
    );
    assert_eq!(env.count("files"), 1);
    assert_eq!(env.count("file_locations"), 1);
    assert_eq!(env.count("file_metadata"), 1);

    // Tracking the path again puts the location back on the same id.
    env.track(&run42);
    assert_eq!(env.id_at(&run42.join("S1.bam")), id);

    // Both copies selected: the file row goes, and that asks first.
    let archive = env.data().join("archive");
    let refused = fail(env.untrack(&[s(&run42), s(&archive)]));
    assert!(refused.err.contains("--yes"), "{}", refused.err);
    assert_eq!(env.count("files"), 1);
    let run = ok(env.untrack(&["--yes", s(&run42), s(&archive)]));
    assert!(run.out.contains("1 files (1 with metadata)"), "{}", run.out);
    assert_eq!(
        env.count("files") + env.count("file_locations") + env.count("file_metadata"),
        0
    );
    assert!(run42.join("S1.bam").exists());
}

#[test]
fn a_path_gone_from_disk_can_be_untracked() {
    let env = Env::new("gone");
    env.write(&["run42/a.fq", "run42/b.fq"], "reads\n");
    env.write(&["run42/c.fq"], "other\n");
    let run42 = env.data().join("run42");
    env.track(&run42);
    fs::remove_dir_all(&run42).unwrap();
    ok(env.untrack(&["--yes", s(&run42)]));
    assert_eq!(env.count("files") + env.count("file_locations"), 0);
    assert_eq!(env.count("file_digests"), 0);
}

#[test]
fn a_linked_file_needs_cascade_and_its_entity_stays() {
    let env = Env::new("linked");
    env.write(&["run42/a.bam"], "a\n");
    env.write(&["run42/b.bam"], "b\n");
    let run42 = env.data().join("run42");
    env.track(&run42);
    let a = run42.join("a.bam");
    ok(env.bpm(&["link", "--yes", "--to", "/P/J", s(&a), "--role", "data"]));

    let refused = fail(env.untrack(&["--yes", s(&run42)]));
    assert!(refused.err.contains(s(&a)), "{}", refused.err);
    assert!(refused.err.contains("--cascade"), "{}", refused.err);
    assert_eq!(env.count("files"), 2);
    assert_eq!(env.count("file_links"), 1);

    ok(env.untrack(&["--yes", "--cascade", s(&run42)]));
    assert_eq!(env.count("files") + env.count("file_links"), 0);
    assert_eq!(env.count("projects"), 1);
}

#[test]
fn a_linked_file_with_another_copy_keeps_its_links() {
    let env = Env::new("linkcopy");
    env.write(&["run42/a.bam", "archive/a.bam"], "a\n");
    env.track(&env.data());
    let run42 = env.data().join("run42");
    let a = run42.join("a.bam");
    ok(env.bpm(&["link", "--yes", "--to", "/P/J", s(&a), "--role", "data"]));
    ok(env.untrack(&[s(&run42)]));
    assert_eq!(env.count("files"), 1);
    assert_eq!(env.count("file_links"), 1);
}

#[test]
fn filters_narrow_what_is_removed() {
    let env = Env::new("filters");
    env.write(&["run42/a.bam"], "a\n");
    env.write(&["run42/a.txt"], "notes\n");
    env.write(&["run42/sub/b.bam"], "b\n");
    let run42 = env.data().join("run42");
    env.track(&run42);

    let run = ok(env.untrack(&["--yes", "--whitelist", "*.bam", s(&run42)]));
    assert!(
        run.out
            .contains("2 locations selected, 1 excluded by filters"),
        "{}",
        run.out
    );
    assert_eq!(env.id_at(&run42.join("a.bam")), "");
    assert_ne!(env.id_at(&run42.join("a.txt")), "");

    // A whitelist in the config narrows it the same way; an empty flag
    // replaces it and reaches the rest.
    fs::create_dir_all(env.scratch.0.join(".bpm")).unwrap();
    fs::write(
        env.scratch.0.join(".bpm/config.toml"),
        "whitelist = [\"*.bam\"]\n",
    )
    .unwrap();
    let run = ok(env.untrack(&["--yes", s(&run42)]));
    assert!(run.out.contains("0 locations selected"), "{}", run.out);
    assert!(run.out.contains("nothing to untrack"), "{}", run.out);
    ok(env.untrack(&["--yes", "--whitelist", "", s(&run42)]));
    assert_eq!(env.count("files"), 0);
}

#[test]
fn dry_run_writes_nothing_and_detail_lists_each_removal() {
    let env = Env::new("dry");
    env.write(&["run42/a.fq", "archive/a.fq"], "a\n");
    env.write(&["run42/b.fq"], "b\n");
    env.track(&env.data());
    let run42 = env.data().join("run42");
    let run = ok(env.untrack(&["-n", "--detail", s(&run42)]));
    let a = run42.join("a.fq");
    let b = run42.join("b.fq");
    assert!(
        run.out.contains(&format!("location\t{}", a.display())),
        "{}",
        run.out
    );
    assert!(
        run.out.contains(&format!("\t{}", b.display())),
        "{}",
        run.out
    );
    assert!(run.out.contains("file\t"), "{}", run.out);
    assert_eq!(env.count("file_locations"), 3);
    assert_eq!(env.count("untrack_runs"), 0);
}

#[test]
fn an_operand_reaches_whole_components_and_must_reach_something() {
    let env = Env::new("reach");
    env.write(&["run1/a.fq"], "a\n");
    env.write(&["run10/b.fq"], "b\n");
    env.track(&env.data());
    ok(env.untrack(&["--yes", s(&env.data().join("run1"))]));
    assert_ne!(env.id_at(&env.data().join("run10/b.fq")), "");
    assert_eq!(env.count("files"), 1);

    // Nothing is written when any operand reaches nothing.
    let refused = fail(env.untrack(&[
        "--yes",
        s(&env.data().join("run10")),
        s(&env.data().join("nowhere")),
    ]));
    assert!(refused.err.contains("nowhere"), "{}", refused.err);
    assert_eq!(env.count("files"), 1);
}

#[test]
fn an_applied_untrack_is_logged() {
    let env = Env::new("log");
    env.write(&["run42/a.fq", "archive/a.fq"], "a\n");
    env.write(&["run42/b.fq"], "b\n");
    env.track(&env.data());
    let run42 = env.data().join("run42");
    let run = ok(env.untrack(&["--yes", s(&run42)]));
    assert!(run.out.starts_with("2 locations selected"), "{}", run.out);
    assert_eq!(
        env.scalar(
            "SELECT locations_removed || ' ' || files_removed || ' ' || links_removed FROM untrack_runs"
        ),
        "1 1 0"
    );
    assert!(
        env.scalar("SELECT command FROM untrack_runs")
            .contains("untrack")
    );
}

#[test]
fn delete_takes_entities_only() {
    let env = Env::new("delete");
    env.write(&["run42/a.fq"], "a\n");
    let run42 = env.data().join("run42");
    env.track(&run42);
    let path = run42.join("a.fq");
    fail(env.bpm(&["delete", "--file", s(&path)]));
    fail(env.bpm(&["delete", "--location", s(&path)]));
    assert_eq!(env.count("files"), 1);
}

#[test]
fn a_link_committed_after_the_plan_stops_the_apply() {
    use bpm3::catalog::Catalog;

    let env = Env::new("race");
    env.write(&["run42/a.bam"], "a\n");
    let run42 = env.data().join("run42");
    env.track(&run42);
    let operands = vec![run42.to_str().unwrap().to_string()];
    let keep = |_: &str, _: &str| true;
    let mut catalog = Catalog::open_write(&env.catalog).unwrap();
    let plan = catalog.plan_untrack(&operands, &keep).unwrap();
    assert_eq!(plan.files.len(), 1);
    ok(env.bpm(&[
        "link",
        "--yes",
        "--to",
        "/P/J",
        s(&run42.join("a.bam")),
        "--role",
        "data",
    ]));
    let err = catalog
        .apply_untrack(&operands, &keep, &plan, true, "[]")
        .unwrap_err();
    assert!(err.to_string().contains("changed"), "{err}");
    assert_eq!(env.count("files") + env.count("file_links"), 2);
}
