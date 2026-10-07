//! Stage 2 acceptance tests: PRD §4.15 scenarios 10–21, local filesystem only.
//! The object-store half of scenario 15 waits for a second backend. Each test
//! uses its own HOME, catalog, and data directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "bpm3-s2-{label}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        // Locations are stored canonical, so compare against canonical paths
        // (on macOS the temp dir sits behind the /var -> /private/var link).
        Self(fs::canonicalize(&path).unwrap())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // An unreadable fixture would stop the cleanup.
        let _ = Command::new("chmod")
            .arg("-R")
            .arg("u+rwx")
            .arg(&self.0)
            .status();
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Run {
    code: i32,
    out: String,
    err: String,
}

/// A HOME, a catalog in it, and a data directory beside it.
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
        env
    }

    fn home(&self) -> &Path {
        &self.scratch.0
    }

    fn data(&self) -> PathBuf {
        self.scratch.0.join("data")
    }

    fn raw(&self, args: &[&str]) -> Run {
        let output = Command::new(env!("CARGO_BIN_EXE_bpm"))
            .current_dir(self.home())
            .env("HOME", self.home())
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

    fn sql(&self, statement: &str) -> String {
        ok(self.bpm(&["sql", statement])).out
    }

    fn scalar(&self, statement: &str) -> String {
        let out = self.sql(statement);
        out.lines().nth(1).unwrap_or("").trim().to_string()
    }

    fn count(&self, table: &str) -> i64 {
        self.scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .parse()
            .unwrap()
    }

    fn ingest(&self, path: &Path) -> Run {
        ok(self.bpm(&["ingest", path.to_str().unwrap()]))
    }

    fn scan(&self, path: &Path) -> Run {
        ok(self.bpm(&["scan", path.to_str().unwrap()]))
    }

    fn files(&self, args: &[&str]) -> Vec<Value> {
        let mut full = vec!["query", "files", "--format", "json"];
        full.extend_from_slice(args);
        let out = ok(self.bpm(&full)).out;
        serde_json::from_str::<Value>(&out)
            .unwrap()
            .as_array()
            .unwrap()
            .clone()
    }

    /// The file a location belongs to, as `query files` reports it.
    fn file_at(&self, path: &Path) -> Value {
        let uri = path.to_str().unwrap();
        self.files(&[])
            .into_iter()
            .find(|file| {
                file["locations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|location| location["uri"] == uri)
            })
            .unwrap_or_else(|| panic!("no file has a location at {uri}"))
    }

    fn id_at(&self, path: &Path) -> String {
        self.file_at(path)["id"].as_str().unwrap().to_string()
    }

    fn location(&self, path: &Path) -> Value {
        let uri = path.to_str().unwrap();
        self.file_at(path)["locations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|location| location["uri"] == uri)
            .unwrap()
            .clone()
    }

    fn drift(&self, path: &Path) -> Vec<String> {
        self.location(path)["drift"]
            .as_array()
            .unwrap()
            .iter()
            .map(|state| state.as_str().unwrap().to_string())
            .collect()
    }

    fn create(&self, args: &[&str]) -> String {
        let mut full = vec!["create"];
        full.extend_from_slice(args);
        ok(self.bpm(&full)).out.trim().to_string()
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

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

fn set_mtime(path: &Path, stamp: &str) {
    assert!(
        Command::new("touch")
            .args(["-t", stamp])
            .arg(path)
            .status()
            .unwrap()
            .success()
    );
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item.as_str().unwrap().to_string())
        .collect()
}

/// The columns ingest must leave alone, for one file.
fn snapshot(env: &Env) -> String {
    env.sql(
        "SELECT f.id, f.size_bytes, f.mtime, f.fingerprint, f.fingerprint_scheme,
                l.uri, l.last_size, l.last_mtime,
                (SELECT COUNT(*) FROM file_digests d WHERE d.file_id = f.id)
         FROM files f JOIN file_locations l ON l.file_id = f.id ORDER BY l.uri",
    )
}

#[test]
fn query_files_count_respects_the_filters_and_the_format() {
    let env = Env::new("count");
    let dir = env.data().join("run");
    write(&dir.join("a.fq"), b"aaa\n");
    write(&dir.join("b.fq"), b"bbb\n");
    write(&dir.join("c.fq"), b"ccc\n");
    env.ingest(&dir);

    let table = ok(env.bpm(&["query", "files", "--count"]));
    assert_eq!(table.out, "count\n3\n");
    let csv = ok(env.bpm(&["query", "files", "-c", "--format", "csv"]));
    assert_eq!(csv.out, "count\n3\n");
    let json = ok(env.bpm(&["query", "files", "--count", "--format", "json"]));
    assert_eq!(
        serde_json::from_str::<Value>(&json.out).unwrap()["count"],
        3
    );

    assert_eq!(
        ok(env.bpm(&["query", "files", "--count", "--unlinked"])).out,
        "count\n3\n"
    );
    assert_eq!(
        ok(env.bpm(&["query", "files", "--count", "--drift", "unverified"])).out,
        "count\n3\n"
    );
    assert_eq!(
        ok(env.bpm(&["query", "files", "--count", "--drift", "ok"])).out,
        "count\n0\n"
    );
    assert_eq!(
        ok(env.bpm(&["query", "files", "--count", "--digest", "blake3:ab"])).out,
        "count\n0\n"
    );

    let id = env.id_at(&dir.join("a.fq"));
    env.create(&["program", "--name", "CLL"]);
    ok(env.bpm(&["link", "--to", "/CLL", &id, "--role", "data"]));
    assert_eq!(
        ok(env.bpm(&["query", "files", "--count", "--role", "data"])).out,
        "count\n1\n"
    );
    assert_eq!(
        ok(env.bpm(&["query", "files", "--count", "--under", "/CLL"])).out,
        "count\n1\n"
    );
    assert_eq!(
        ok(env.bpm(&["query", "files", "--count", "--unlinked"])).out,
        "count\n2\n"
    );
    assert_eq!(env.files(&["--unlinked"]).len(), 2);
}

#[test]
fn scenario_10_ingest_creates_unlinked_rows_without_payload() {
    let env = Env::new("s10");
    let run42 = env.data().join("run42");
    let marker = b"PAYLOAD-7d1c0e2a-must-not-reach-the-catalog\n";
    write(&run42.join("S1_R1.fq.gz"), marker);
    write(&run42.join("lane1/S1_R2.fq.gz"), b"ACGTACGT\n");

    let run = env.ingest(&run42);
    assert!(run.out.contains("2 new files"), "{}", run.out);
    let files = env.files(&[]);
    assert_eq!(files.len(), 2);
    for file in &files {
        assert!(file["links"].as_array().unwrap().is_empty());
        assert!(file["size"].as_i64().unwrap() > 0);
        assert!(file["mtime"].as_str().unwrap().ends_with('Z'));
        assert_eq!(file["fingerprint"]["scheme"], "xxh3-128-full");
        assert_eq!(file["fingerprint"]["hex"].as_str().unwrap().len(), 32);
        assert_eq!(file["locations"].as_array().unwrap().len(), 1);
        assert_eq!(file["locations"][0]["backend"], "posix");
        // Ingest does not hash a file that has no possible duplicate.
        assert!(file["digests"].as_array().unwrap().is_empty());
        assert_eq!(strings(&file["drift"]), ["unverified"]);
    }
    let r1 = env.file_at(&run42.join("S1_R1.fq.gz"));
    assert_eq!(r1["size"], marker.len());
    assert_eq!(env.files(&["--unlinked"]).len(), 2);

    for suffix in ["", "-wal"] {
        let mut name = env.catalog.as_os_str().to_owned();
        name.push(suffix);
        if let Ok(bytes) = fs::read(PathBuf::from(name)) {
            assert!(
                !bytes.windows(marker.len()).any(|window| window == marker),
                "file bytes were written into the catalog"
            );
        }
    }
    assert_eq!(
        env.scalar("SELECT status || ' ' || files_seen || ' ' || files_created FROM ingest_runs"),
        "complete 2 2"
    );
}

#[test]
fn scenario_11_second_ingest_changes_nothing() {
    let env = Env::new("s11");
    let run42 = env.data().join("run42");
    write(&run42.join("a.fq"), b"aaaa\n");
    write(&run42.join("b.fq"), b"bbbbbb\n");
    env.ingest(&run42);
    env.scan(&run42);
    let before = snapshot(&env);
    let run = env.ingest(&run42);
    assert!(run.out.contains("0 new files"), "{}", run.out);
    assert_eq!(snapshot(&env), before);
    assert_eq!(env.count("files"), 2);
    assert_eq!(env.count("ingest_runs"), 2);
}

#[test]
fn scenario_12_ingest_ignores_existing_drift() {
    let env = Env::new("s12");
    let run42 = env.data().join("run42");
    let fq = run42.join("S1_R1.fq.gz");
    write(&fq, b"original bytes\n");
    env.ingest(&run42);
    let before = snapshot(&env);

    write(&fq, b"original bytes, and more appended overnight\n");
    env.ingest(&run42);
    assert_eq!(snapshot(&env), before);
    assert_eq!(env.count("files"), 1);

    env.scan(&run42);
    assert_eq!(env.drift(&fq), ["stat_changed"]);
}

#[test]
fn scenario_13_duplicate_after_scan_joins_the_existing_file() {
    let env = Env::new("s13a");
    let run42 = env.data().join("run42");
    let archive = env.data().join("archive");
    write(&run42.join("S1_R1.fq.gz"), b"same bytes\n");
    write(&archive.join("S1_R1.fq.gz"), b"same bytes\n");
    env.ingest(&run42);
    env.scan(&run42);
    let id = env.id_at(&run42.join("S1_R1.fq.gz"));

    // A linked file takes a second location the same way.
    env.create(&["program", "--name", "CLL"]);
    ok(env.bpm(&["link", "--to", "/CLL", &id, "--role", "data"]));

    let run = env.ingest(&archive);
    assert!(
        run.out.contains("0 new files, 1 new locations"),
        "{}",
        run.out
    );
    assert_eq!(env.id_at(&archive.join("S1_R1.fq.gz")), id);
    assert_eq!(env.count("files"), 1);
    assert_eq!(env.drift(&archive.join("S1_R1.fq.gz")), ["ok"]);
}

#[test]
fn ingest_logs_its_start_and_counts_every_hundred_files() {
    let env = Env::new("progress");
    let one = env.data().join("one");
    write(&one.join("notes.txt"), b"notes\n");
    let quiet = env.ingest(&one);
    assert!(
        quiet
            .err
            .contains(&format!("bpm: ingest {}", one.display())),
        "{}",
        quiet.err
    );
    assert!(
        quiet.err.contains("bpm: blacklist .DS_Store, Thumbs.db"),
        "{}",
        quiet.err
    );
    assert!(!quiet.err.contains("whitelist"), "{}", quiet.err);
    assert!(!quiet.err.contains("files seen"), "{}", quiet.err);

    let many = env.data().join("many");
    for index in 0..100 {
        write(
            &many.join(format!("f{index:03}.png")),
            format!("{index}\n").as_bytes(),
        );
    }
    write(&many.join("skip.txt"), b"no");
    let run = ok(env.bpm(&[
        "ingest",
        many.to_str().unwrap(),
        "--whitelist",
        "*.png",
        "--blacklist",
        "*.tmp",
        "--no-default-blacklist",
    ]));
    assert!(
        run.err.contains(&format!("bpm: ingest {}", many.display())),
        "{}",
        run.err
    );
    assert!(run.err.contains("bpm: whitelist *.png"), "{}", run.err);
    assert!(run.err.contains("bpm: blacklist *.tmp"), "{}", run.err);
    // The test captures stderr through a pipe, so the count is a plain line.
    // A terminal rewrites that line instead; see paint_progress.
    assert!(run.err.contains("bpm: 100 files seen"), "{}", run.err);
    assert!(!run.err.contains('\u{1b}'), "{}", run.err);
    assert!(!run.err.contains("bpm: 200 files seen"), "{}", run.err);
    assert!(!run.err.contains("skip.txt"), "{}", run.err);
    assert!(
        run.out.contains("100 files seen, 100 new files"),
        "{}",
        run.out
    );
    assert_eq!(run.out.lines().filter(|line| !line.is_empty()).count(), 1);
}

#[test]
fn ingest_summary_counts_copies_without_listing_paths() {
    let env = Env::new("copies");
    let pics = env.data().join("pics");
    let bytes = b"same bytes\n";
    write(&pics.join("a.png"), bytes);
    write(&pics.join("b.png"), bytes);
    write(&pics.join("c.png"), bytes);
    write(&pics.join("other.png"), b"different\n");

    let run = env.ingest(&pics);
    assert!(
        run.out
            .contains("4 files seen, 2 new files, 4 new locations, 2 copies, 0 already recorded"),
        "{}",
        run.out
    );
    // The summary carries the count. Paths stay in the catalog, not on the console.
    assert_eq!(run.out.lines().filter(|line| !line.is_empty()).count(), 1);
    let a = pics.join("a.png");
    assert_eq!(env.count("files"), 2);
    assert_eq!(env.file_at(&a)["locations"].as_array().unwrap().len(), 3);

    let more = env.data().join("more");
    write(&more.join("d.png"), bytes);
    let again = env.ingest(&more);
    assert!(
        again
            .out
            .contains("1 files seen, 0 new files, 1 new locations, 1 copies, 0 already recorded"),
        "{}",
        again.out
    );
    assert!(!again.out.contains("d.png"), "{}", again.out);

    let repeat = env.ingest(&pics);
    assert!(
        repeat
            .out
            .contains("4 files seen, 0 new files, 0 new locations, 0 copies, 4 already recorded"),
        "{}",
        repeat.out
    );
}

#[test]
fn scenario_13_duplicate_before_any_blake3_hashes_both_copies() {
    let env = Env::new("s13b");
    let run42 = env.data().join("run42");
    let archive = env.data().join("archive");
    write(&run42.join("S1_R1.fq.gz"), b"same bytes\n");
    write(&archive.join("S1_R1.fq.gz"), b"same bytes\n");
    env.ingest(&run42);
    assert_eq!(env.count("file_digests"), 0);

    env.ingest(&archive);
    let file = env.file_at(&run42.join("S1_R1.fq.gz"));
    assert_eq!(env.count("files"), 1);
    assert_eq!(file["locations"].as_array().unwrap().len(), 2);
    let digests = file["digests"].as_array().unwrap();
    assert_eq!(digests.len(), 1);
    assert!(
        digests[0]["digest"]
            .as_str()
            .unwrap()
            .starts_with("blake3:")
    );
    // Both copies were read, so both are verified against that digest.
    assert_eq!(strings(&file["drift"]), ["ok"]);
}

#[test]
fn same_size_and_fingerprint_with_different_bytes_is_not_merged() {
    // Fingerprint equality alone never merges. Here the fingerprints differ
    // too, so ingest does not even hash; a sampled pair is in the unit tests.
    let env = Env::new("nomerge");
    let run42 = env.data().join("run42");
    write(&run42.join("a.fq"), b"AAAA\n");
    write(&run42.join("b.fq"), b"BBBB\n");
    env.ingest(&run42);
    assert_eq!(env.count("files"), 2);
    assert_eq!(env.count("file_digests"), 0);
}

#[test]
#[cfg(unix)]
fn scenario_14_an_unreadable_file_is_reported_and_not_fingerprinted() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new("s14");
    let run42 = env.data().join("run42");
    let locked = run42.join("locked.fq");
    write(&locked, b"secret\n");
    write(&run42.join("open.fq"), b"open\n");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(&locked).is_ok() {
        eprintln!("skipping: this user can read a mode 000 file");
        return;
    }
    let run = fail(env.bpm(&["ingest", run42.to_str().unwrap()]));
    assert!(run.err.contains("locked.fq"), "{}", run.err);
    assert!(run.out.contains("1 errors"), "{}", run.out);
    // The readable file is still committed, and the run is complete.
    assert!(env.file_at(&run42.join("open.fq"))["fingerprint"].is_object());
    assert!(env.file_at(&locked)["fingerprint"].is_null());
    assert_eq!(env.count("ingest_errors"), 1);
    assert_eq!(env.scalar("SELECT status FROM ingest_runs"), "complete");
}

#[test]
fn scenario_15_stat_drift_and_backend_selection() {
    let env = Env::new("s15");
    let run42 = env.data().join("run42");
    let fq = run42.join("S1_R1.fq.gz");
    write(&fq, b"0123456789\n");
    env.ingest(&run42);
    env.scan(&run42);
    let id = env.id_at(&fq);
    assert_eq!(env.drift(&fq), ["ok"]);

    write(&fq, b"01234\n");
    env.scan(&run42);
    assert_eq!(env.id_at(&fq), id);
    assert!(env.drift(&fq).contains(&"stat_changed".to_string()));

    // A backend argument narrows the scan to that backend. No s3 location
    // exists in this milestone, so it selects nothing.
    let posix = ok(env.bpm(&["scan", "posix"]));
    assert!(posix.out.contains(": 1 locations"), "{}", posix.out);
    let s3 = ok(env.bpm(&["scan", "s3"]));
    assert!(s3.out.contains(": 0 locations"), "{}", s3.out);
    let all = ok(env.bpm(&["scan"]));
    assert!(all.out.contains(": 1 locations"), "{}", all.out);
    // A path outside every location selects nothing either.
    let elsewhere = ok(env.bpm(&["scan", env.data().join("nowhere").to_str().unwrap()]));
    assert!(elsewhere.out.contains(": 0 locations"), "{}", elsewhere.out);
}

#[test]
fn stat_changed_stays_until_acknowledged() {
    let env = Env::new("sticky");
    let run42 = env.data().join("run42");
    let fq = run42.join("a.fq");
    write(&fq, b"abc\n");
    env.ingest(&run42);
    write(&fq, b"abcdef\n");
    env.scan(&run42);
    env.scan(&run42);
    assert_eq!(env.drift(&fq), ["stat_changed"]);
    // The first read records a digest even though the stat changed, so a
    // later change to the bytes is still caught.
    assert_eq!(env.count("file_digests"), 1);
    write(&fq, b"abcxyz\n");
    set_mtime(&fq, "202001010000");
    env.scan(&run42);
    assert_eq!(env.drift(&fq), ["stat_changed", "digest_mismatch"]);

    let ack = ok(env.bpm(&["acknowledge", fq.to_str().unwrap()]));
    assert!(ack.out.contains("generation 2 is current"), "{}", ack.out);
    assert_eq!(env.drift(&fq), ["ok"]);
    env.scan(&run42);
    assert_eq!(env.drift(&fq), ["ok"]);

    // A stat change with the same bytes is accepted without a new generation.
    set_mtime(&fq, "202101010000");
    env.scan(&run42);
    assert_eq!(env.drift(&fq), ["stat_changed"]);
    let ack = ok(env.bpm(&["acknowledge", fq.to_str().unwrap()]));
    assert!(ack.out.contains("stat accepted"), "{}", ack.out);
    assert_eq!(env.drift(&fq), ["ok"]);
    assert_eq!(env.count("file_digests"), 2);
}

#[test]
fn a_drifted_copy_does_not_hide_its_siblings_from_ingest() {
    let env = Env::new("sibling");
    let run42 = env.data().join("run42");
    let a = run42.join("a.fq");
    let b = run42.join("b.fq");
    write(&a, b"the accepted bytes\n");
    write(&b, b"the accepted bytes\n");
    env.ingest(&run42);
    env.scan(&run42);
    let id = env.id_at(&a);
    let before = env.sql("SELECT size_bytes, mtime, fingerprint FROM files");

    write(&a, b"trunc\n");
    env.scan(&run42);
    assert!(env.drift(&a).contains(&"digest_mismatch".to_string()));
    assert_eq!(
        env.sql("SELECT size_bytes, mtime, fingerprint FROM files"),
        before
    );

    // Another copy of the accepted bytes still joins the same file.
    let archive = env.data().join("archive");
    write(&archive.join("c.fq"), b"the accepted bytes\n");
    env.ingest(&archive);
    assert_eq!(env.id_at(&archive.join("c.fq")), id);
    assert_eq!(env.count("files"), 1);
}

#[test]
fn a_first_digest_comes_from_an_unchanged_copy() {
    let env = Env::new("firstcopy");
    let run42 = env.data().join("run42");
    // "a" sorts first, and is the copy that changes.
    let a = run42.join("a.fq");
    let b = run42.join("b.fq");
    write(&a, b"copy\n");
    write(&b, b"copy\n");
    env.ingest(&run42);
    // Ingest hashed both to merge them; forget that to test scan alone.
    ok(env.bpm(&["sql", "--write", "DELETE FROM file_digests"]));
    ok(env.bpm(&[
        "sql",
        "--write",
        "UPDATE file_locations SET digest_state = 'unverified'",
    ]));
    write(&a, b"CHANGED\n");
    env.scan(&run42);
    assert_eq!(env.drift(&b), ["ok"]);
    assert_eq!(env.drift(&a), ["stat_changed", "digest_mismatch"]);
}

#[test]
fn ingest_and_scan_do_not_need_home_when_the_catalog_is_named() {
    let env = Env::new("nohome");
    write(&env.data().join("a.fq"), b"a\n");
    for args in [
        vec!["ingest", env.data().to_str().unwrap()],
        vec!["scan", env.data().to_str().unwrap()],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_bpm"))
            .current_dir(env.home())
            .env_remove("HOME")
            .env_remove("BPM_CATALOG")
            .arg("--catalog")
            .arg(&env.catalog)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(env.count("file_digests"), 1);
}

#[test]
fn a_new_file_already_committed_by_another_ingest_becomes_a_location() {
    use bpm3::catalog::{AckObservation, Catalog, IngestEntry, RunKind, Seen};
    use bpm3::model::{FileRef, Fingerprint};

    let env = Env::new("race");
    let first = env.data().join("first/x.fq");
    let second = env.data().join("second/x.fq");
    write(&first, b"same\n");
    write(&second, b"same\n");
    env.ingest(first.parent().unwrap());
    env.scan(first.parent().unwrap());
    let id = env.id_at(&first);
    let blake3 = env.scalar("SELECT digest FROM file_digests");

    // A second ingest that hashed the copy before the first committed.
    let mut catalog = Catalog::open_write(&env.catalog).unwrap();
    let run = catalog
        .start_run(RunKind::Ingest, "posix", "/second")
        .unwrap();
    let entry = IngestEntry::New {
        fingerprint: Some(Fingerprint {
            scheme: "xxh3-128-full".into(),
            hex: "0".repeat(32),
        }),
        blake3: Some(blake3.clone()),
        seen: vec![Seen {
            uri: second.to_str().unwrap().into(),
            size: 5,
            mtime: None,
        }],
    };
    let applied = catalog.apply_ingest(&run, &[entry], &[]).unwrap();
    assert_eq!((applied.created, applied.located), (0, 1));
    catalog.finish_run(run, true, 1, 0).unwrap();

    // The catalog refuses an acknowledge with nothing read instead of panicking.
    let empty = AckObservation {
        file_id: id.parse().unwrap(),
        blake3,
        md5: None,
        fingerprint: Fingerprint {
            scheme: "xxh3-128-full".into(),
            hex: "0".repeat(32),
        },
        present: Vec::new(),
        missing: Vec::new(),
    };
    assert!(catalog.acknowledge(&empty).is_err());
    let file = catalog.file(&FileRef::Id(id.parse().unwrap())).unwrap();
    assert_eq!(file.locations.len(), 2);
    drop(catalog);
    assert_eq!(env.count("files"), 1);
}

#[test]
fn scenario_16_digest_drift_and_acknowledge() {
    let env = Env::new("s16");
    let run42 = env.data().join("run42");
    let fq = run42.join("S1_R1.fq.gz");
    let other = run42.join("S2_R1.fq.gz");
    write(&fq, b"generation one\n");
    write(&other, b"second file\n");
    env.ingest(&run42);
    env.scan(&run42);
    let id = env.id_at(&fq);
    let first = env.file_at(&fq)["digests"][0]["digest"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(first.starts_with("blake3:"));

    // Same size, different bytes, and an mtime that is clearly different.
    write(&fq, b"generation two\n");
    write(&other, b"second FILE\n");
    set_mtime(&fq, "202001010000");
    set_mtime(&other, "202001010000");
    env.scan(&run42);
    assert!(env.drift(&fq).contains(&"digest_mismatch".to_string()));
    assert!(env.drift(&other).contains(&"digest_mismatch".to_string()));

    // Acknowledge takes one file. A directory is refused and changes nothing.
    let before = env.sql("SELECT * FROM file_digests ORDER BY file_id, generation");
    let refused = fail(env.bpm(&["acknowledge", run42.to_str().unwrap()]));
    assert!(refused.err.contains("one file"), "{}", refused.err);
    fail(env.bpm(&["acknowledge"]));
    fail(env.bpm(&["acknowledge", fq.to_str().unwrap(), other.to_str().unwrap()]));
    assert_eq!(
        env.sql("SELECT * FROM file_digests ORDER BY file_id, generation"),
        before
    );

    ok(env.bpm(&["acknowledge", &id]));
    let file = env.file_at(&fq);
    assert_eq!(file["id"], id.as_str());
    let current = file["digests"][0]["digest"].as_str().unwrap();
    assert_ne!(current, first);
    assert_eq!(file["digests"][0]["generation"], 2);
    assert_eq!(env.drift(&fq), ["ok"]);
    // The old digest stays as history.
    assert_eq!(
        env.scalar(&format!(
            "SELECT 'blake3:' || digest || ' ' || generation || ' ' || current FROM file_digests
             WHERE file_id = '{id}' AND generation = 1"
        )),
        format!("{first} 1 0")
    );
    let found = env.files(&["--digest", current]);
    assert_eq!(found.len(), 1);
    assert!(env.files(&["--digest", &first]).is_empty());

    // The other drifted file was not touched.
    assert!(env.drift(&other).contains(&"digest_mismatch".to_string()));
    assert_eq!(env.files(&["--drift", "digest_mismatch"]).len(), 1);

    // Acknowledging again with nothing drifted writes nothing.
    let again = ok(env.bpm(&["acknowledge", fq.to_str().unwrap()]));
    assert!(
        again.out.contains("nothing to acknowledge"),
        "{}",
        again.out
    );
    assert_eq!(
        env.scalar(&format!(
            "SELECT COUNT(*) FROM file_digests WHERE file_id = '{id}'"
        )),
        "2"
    );
}

#[test]
fn acknowledge_refuses_locations_that_disagree() {
    let env = Env::new("disagree");
    let run42 = env.data().join("run42");
    let a = run42.join("a.fq");
    let b = run42.join("b.fq");
    write(&a, b"copy\n");
    write(&b, b"copy\n");
    env.ingest(&run42);
    assert_eq!(env.count("files"), 1);
    write(&a, b"COPY\n");
    set_mtime(&a, "202001010000");
    env.scan(&run42);
    let before = env.sql("SELECT * FROM file_digests");
    let refused = fail(env.bpm(&["acknowledge", a.to_str().unwrap()]));
    assert!(
        refused.err.contains("do not hold the same bytes"),
        "{}",
        refused.err
    );
    assert!(refused.err.contains(b.to_str().unwrap()), "{}", refused.err);
    assert_eq!(env.sql("SELECT * FROM file_digests"), before);
    assert!(env.drift(&a).contains(&"digest_mismatch".to_string()));
}

#[test]
fn scenario_17_move_with_a_digest() {
    let env = Env::new("s17");
    let run42 = env.data().join("run42");
    let archive = env.data().join("archive");
    let old = run42.join("S1_R1.fq.gz");
    let new = archive.join("S1_R1.fq.gz");
    write(&old, b"hashed before the move\n");
    env.ingest(&run42);
    env.scan(&run42);
    let id = env.id_at(&old);

    fs::create_dir_all(&archive).unwrap();
    fs::rename(&old, &new).unwrap();
    env.ingest(&archive);
    assert_eq!(env.id_at(&new), id);
    let scan = env.scan(&run42);
    assert!(scan.out.contains("missing"), "{}", scan.out);
    assert_eq!(env.drift(&old), ["missing"]);
    assert_eq!(env.drift(&new), ["ok"]);
    assert_eq!(env.files(&["--drift", "missing"]).len(), 1);

    // The old location stays until the operator removes it.
    env.scan(&run42);
    assert_eq!(env.file_at(&new)["locations"].as_array().unwrap().len(), 2);
    ok(env.bpm(&["delete", "--location", old.to_str().unwrap()]));
    let file = env.file_at(&new);
    assert_eq!(file["id"], id.as_str());
    assert_eq!(file["locations"].as_array().unwrap().len(), 1);
    fail(env.bpm(&["delete", "--location", old.to_str().unwrap()]));
}

#[test]
fn scenario_18_move_without_a_readable_source() {
    let env = Env::new("s18");
    let run42 = env.data().join("run42");
    let archive = env.data().join("archive");
    let old = run42.join("S1_R1.fq.gz");
    let new = archive.join("S1_R1.fq.gz");
    write(&old, b"never hashed\n");
    env.ingest(&run42);
    let id = env.id_at(&old);

    fs::create_dir_all(&archive).unwrap();
    fs::rename(&old, &new).unwrap();
    env.scan(&run42);
    assert_eq!(env.drift(&old), ["missing"]);
    let run = env.ingest(&archive);
    assert!(run.out.contains("possible duplicate"), "{}", run.out);
    assert!(run.out.contains(&id), "{}", run.out);
    assert_ne!(env.id_at(&new), id);
    assert_eq!(env.count("files"), 2);
}

#[test]
fn scenario_19_two_copies_are_one_file() {
    let env = Env::new("s19");
    // Copies in one ingest, in one batch.
    let together = env.data().join("together");
    write(&together.join("a/x.bam"), b"bam bytes\n");
    write(&together.join("b/x.bam"), b"bam bytes\n");
    env.ingest(&together);
    let file = env.file_at(&together.join("a/x.bam"));
    assert_eq!(file["locations"].as_array().unwrap().len(), 2);
    assert_eq!(strings(&file["drift"]), ["ok"]);
    assert_eq!(env.count("files"), 1);

    // Copies in two ingests.
    let first = env.data().join("first");
    let second = env.data().join("second");
    write(&first.join("y.bam"), b"other bam bytes\n");
    write(&second.join("y.bam"), b"other bam bytes\n");
    env.ingest(&first);
    env.ingest(&second);
    assert_eq!(
        env.id_at(&first.join("y.bam")),
        env.id_at(&second.join("y.bam"))
    );
    assert_eq!(env.count("files"), 2);
}

#[test]
fn scenario_20_link_query_and_unlink() {
    let env = Env::new("s20");
    let run42 = env.data().join("run42");
    let fq = run42.join("S1_R1.fq.gz");
    write(&fq, b"reads\n");
    write(&run42.join("unrelated.fq"), b"other\n");
    env.ingest(&run42);
    env.create(&["program", "--name", "CLL"]);
    env.create(&["project", "--parent", "/CLL", "--name", "WES"]);
    let case = env.create(&["case", "--parent", "/CLL/WES"]);
    let sample = env.create(&["sample", "--parent", &case]);
    let raw = env.create(&["raw_data", "--parent", &sample]);
    let other_case = env.create(&["case", "--parent", "/CLL/WES"]);

    ok(env.bpm(&["link", "--to", &raw, fq.to_str().unwrap(), "--role", "data"]));
    let under = env.files(&["--under", &case]);
    assert_eq!(under.len(), 1);
    assert_eq!(under[0]["links"][0]["role"], "data");
    assert_eq!(under[0]["links"][0]["node_type"], "raw_data");
    assert_eq!(
        env.files(&["--under", "/CLL/WES", "--role", "data"]).len(),
        1
    );
    assert!(
        env.files(&["--under", "/CLL/WES", "--role", "index"])
            .is_empty()
    );
    assert!(env.files(&["--under", &other_case]).is_empty());

    let id = env.id_at(&fq);
    ok(env.bpm(&[
        "link",
        "--to",
        &other_case,
        &id,
        "--role",
        "control",
        "--new-role",
    ]));
    assert_eq!(env.files(&["--under", &other_case]).len(), 1);
    assert_eq!(env.file_at(&fq)["links"].as_array().unwrap().len(), 2);

    ok(env.bpm(&["unlink", "--to", &raw, &id]));
    assert!(env.files(&["--under", &case]).is_empty());
    assert_eq!(env.files(&["--under", &other_case]).len(), 1);
    fail(env.bpm(&["unlink", "--to", &raw, &id]));
    // A link needs a file the catalog knows and a role.
    fail(env.bpm(&[
        "link",
        "--to",
        &raw,
        env.data().join("nope").to_str().unwrap(),
        "--role",
        "data",
    ]));
    fail(env.bpm(&["link", "--to", &raw, &id, "--role", ""]));
}

#[test]
fn scenario_21_unlink_and_cascade_delete_keep_files_and_bytes() {
    let env = Env::new("s21");
    let run42 = env.data().join("run42");
    let fq = run42.join("S1_R1.fq.gz");
    write(&fq, b"reads\n");
    env.ingest(&run42);
    env.create(&["program", "--name", "CLL"]);
    env.create(&["project", "--parent", "/CLL", "--name", "WES"]);
    let case = env.create(&["case", "--parent", "/CLL/WES"]);
    let id = env.id_at(&fq);

    ok(env.bpm(&["link", "--to", &case, &id, "--role", "data"]));
    ok(env.bpm(&["unlink", "--to", &case, &id]));
    assert_eq!(env.count("files"), 1);
    assert_eq!(env.files(&["--unlinked"]).len(), 1);

    ok(env.bpm(&["link", "--to", &case, &id, "--role", "data"]));
    fail(env.bpm(&["delete", "/CLL"]));
    ok(env.bpm(&["delete", "--cascade", "/CLL"]));
    assert_eq!(
        env.count("programs") + env.count("projects") + env.count("cases"),
        0
    );
    assert_eq!(env.count("file_links"), 0);
    assert_eq!(env.files(&["--unlinked"]).len(), 1);
    assert_eq!(fs::read(&fq).unwrap(), b"reads\n");

    // A file row can be removed too; its bytes stay.
    ok(env.bpm(&["delete", "--file", fq.to_str().unwrap()]));
    assert_eq!(env.count("files") + env.count("file_locations"), 0);
    assert!(fq.exists());
}

#[test]
fn deleting_a_linked_file_needs_cascade() {
    let env = Env::new("delfile");
    let run42 = env.data().join("run42");
    write(&run42.join("a.fq"), b"a\n");
    env.ingest(&run42);
    env.create(&["program", "--name", "CLL"]);
    let id = env.id_at(&run42.join("a.fq"));
    ok(env.bpm(&["link", "--to", "/CLL", &id, "--role", "data"]));
    let refused = fail(env.bpm(&["delete", "--file", &id]));
    assert!(refused.err.contains("links"), "{}", refused.err);
    ok(env.bpm(&["delete", "--file", &id, "--cascade"]));
    assert_eq!(env.count("files") + env.count("file_links"), 0);
    assert_eq!(env.count("programs"), 1);
}

#[test]
fn scan_md5_is_a_second_digest_of_the_same_generation() {
    let env = Env::new("md5");
    let run42 = env.data().join("run42");
    let fq = run42.join("a.fq");
    write(&fq, b"ACGT\n");
    env.ingest(&run42);
    env.scan(&run42);
    assert_eq!(env.count("file_digests"), 1);
    ok(env.bpm(&["scan", "--md5", run42.to_str().unwrap()]));
    assert_eq!(
        env.sql(
            "SELECT algorithm, digest, generation, current FROM file_digests ORDER BY algorithm"
        ),
        env.sql(&format!(
            "SELECT 'blake3' AS algorithm, '{}' AS digest, 1 AS generation, 1 AS current
             UNION ALL SELECT 'md5', '58ce66d7df0a1cf9b360cabf43da3ea5', 1, 1",
            blake3_hex(b"ACGT\n")
        ))
    );
    assert_eq!(
        env.files(&["--digest", "MD5:58CE66D7DF0A1CF9B360CABF43DA3EA5"])
            .len(),
        1
    );

    // Acknowledge keeps MD5 current for the new bytes, in the new generation.
    write(&fq, b"TTTT\n");
    set_mtime(&fq, "202001010000");
    env.scan(&run42);
    ok(env.bpm(&["acknowledge", fq.to_str().unwrap()]));
    assert_eq!(
        env.scalar("SELECT group_concat(algorithm || generation, ',') FROM (SELECT * FROM file_digests WHERE current = 1 ORDER BY algorithm)"),
        "blake32,md52"
    );
}

fn blake3_hex(bytes: &[u8]) -> String {
    // The CLI is the only hasher the tests use; ask it through a scratch catalog.
    let env = Env::new("b3");
    write(&env.data().join("x"), bytes);
    env.ingest(&env.data());
    env.scan(&env.data());
    env.scalar("SELECT digest FROM file_digests")
}

#[test]
fn path_filters_apply_to_ingest_and_scan() {
    let env = Env::new("filters");
    let run42 = env.data().join("run42");
    for name in [
        "S1.fq.gz",
        ".DS_Store",
        "deep/Thumbs.db",
        "notes.txt",
        "scratch/tmp.fq.gz",
        "keep/scratch/x.fq.gz",
        "old.bak",
    ] {
        write(&run42.join(name), name.as_bytes());
    }
    fs::create_dir_all(env.home().join(".bpm")).unwrap();
    fs::write(
        env.home().join(".bpm/config.toml"),
        "blacklist = [\"*.txt\", \"scratch/**\"]\n",
    )
    .unwrap();

    let uris = |env: &Env| -> Vec<String> {
        let mut uris: Vec<String> = env
            .files(&[])
            .iter()
            .flat_map(|file| {
                strings(
                    &file["locations"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|l| l["uri"].clone())
                        .collect(),
                )
            })
            .map(|uri| {
                uri.strip_prefix(&format!("{}/", run42.display()))
                    .unwrap()
                    .to_string()
            })
            .collect();
        uris.sort();
        uris
    };

    // A whitelist that matches nothing ingests nothing.
    let none = env.bpm(&["ingest", run42.to_str().unwrap(), "--whitelist", "*.cram"]);
    assert!(ok(none).out.contains("0 files seen"));

    env.ingest(&run42);
    assert_eq!(uris(&env), ["S1.fq.gz", "keep/scratch/x.fq.gz", "old.bak"]);

    // --blacklist replaces the global list for one run; built-ins still apply.
    ok(env.bpm(&["ingest", run42.to_str().unwrap(), "--blacklist", "*.bak"]));
    assert_eq!(
        uris(&env),
        [
            "S1.fq.gz",
            "keep/scratch/x.fq.gz",
            "notes.txt",
            "old.bak",
            "scratch/tmp.fq.gz"
        ]
    );
    ok(env.bpm(&["ingest", run42.to_str().unwrap(), "--no-default-blacklist"]));
    assert_eq!(uris(&env).len(), 7);

    // Scan applies the same filters to the locations it checks.
    let scan = ok(env.bpm(&["scan", run42.to_str().unwrap(), "--whitelist", "*.fq.gz"]));
    assert!(scan.out.contains(": 2 locations"), "{}", scan.out);
    let scan = ok(env.bpm(&["scan", run42.to_str().unwrap()]));
    assert!(scan.out.contains(": 3 locations"), "{}", scan.out);

    fs::write(
        env.home().join(".bpm/config.toml"),
        "blacklist = \"*.txt\"\n",
    )
    .unwrap();
    let bad = fail(env.bpm(&["ingest", run42.to_str().unwrap()]));
    assert!(bad.err.contains("config.toml"), "{}", bad.err);
}

#[test]
fn a_config_whitelist_limits_ingest_until_a_flag_replaces_it() {
    let env = Env::new("config-whitelist");
    let run42 = env.data().join("run42");
    for name in [
        "S1.fq.gz",
        "S1.bam",
        "notes.txt",
        "old.bak",
        "scratch/tmp.fq.gz",
        "keep/scratch/x.fq.gz",
        ".DS_Store",
    ] {
        write(&run42.join(name), name.as_bytes());
    }
    fs::create_dir_all(env.home().join(".bpm")).unwrap();
    fs::write(
        env.home().join(".bpm/config.toml"),
        "blacklist = [\"scratch/**\"]\nwhitelist = [\"*.fq.gz\"]\n",
    )
    .unwrap();

    let uris = |env: &Env| -> Vec<String> {
        let mut uris: Vec<String> = env
            .files(&[])
            .iter()
            .flat_map(|file| {
                strings(
                    &file["locations"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|l| l["uri"].clone())
                        .collect(),
                )
            })
            .map(|uri| {
                uri.strip_prefix(&format!("{}/", run42.display()))
                    .unwrap()
                    .to_string()
            })
            .collect();
        uris.sort();
        uris
    };

    env.ingest(&run42);
    assert_eq!(uris(&env), ["S1.fq.gz", "keep/scratch/x.fq.gz"]);
    let scan = ok(env.bpm(&["scan", run42.to_str().unwrap()]));
    assert!(scan.out.contains(": 2 locations"), "{}", scan.out);

    // --whitelist replaces the config list. The config blacklist still applies.
    ok(env.bpm(&["ingest", run42.to_str().unwrap(), "--whitelist", "*.bam"]));
    assert_eq!(uris(&env), ["S1.bam", "S1.fq.gz", "keep/scratch/x.fq.gz"]);
    // A later scan with no flag is back on the config whitelist, so the bam is not checked.
    let scan = ok(env.bpm(&["scan", run42.to_str().unwrap()]));
    assert!(scan.out.contains(": 2 locations"), "{}", scan.out);
    let scan = ok(env.bpm(&["scan", run42.to_str().unwrap(), "--whitelist", ""]));
    assert!(scan.out.contains(": 3 locations"), "{}", scan.out);
    let scan = ok(env.bpm(&["scan", run42.to_str().unwrap(), "--whitelist", "*.bam"]));
    assert!(scan.out.contains(": 1 locations"), "{}", scan.out);

    // --blacklist replaces only the blacklist. scratch/** is gone; *.fq.gz remains.
    ok(env.bpm(&["ingest", run42.to_str().unwrap(), "--blacklist", ""]));
    assert_eq!(
        uris(&env),
        [
            "S1.bam",
            "S1.fq.gz",
            "keep/scratch/x.fq.gz",
            "scratch/tmp.fq.gz"
        ]
    );

    // --whitelist '' clears the config whitelist. Built-ins and scratch/** return.
    ok(env.bpm(&["ingest", run42.to_str().unwrap(), "--whitelist", ""]));
    assert_eq!(
        uris(&env),
        [
            "S1.bam",
            "S1.fq.gz",
            "keep/scratch/x.fq.gz",
            "notes.txt",
            "old.bak",
            "scratch/tmp.fq.gz"
        ]
    );

    // Lifting the built-in names does not lift the config whitelist.
    ok(env.bpm(&["ingest", run42.to_str().unwrap(), "--no-default-blacklist"]));
    assert!(!uris(&env).iter().any(|uri| uri == ".DS_Store"));
    ok(env.bpm(&[
        "ingest",
        run42.to_str().unwrap(),
        "--no-default-blacklist",
        "--whitelist",
        "",
    ]));
    assert!(uris(&env).iter().any(|uri| uri == ".DS_Store"));

    fs::write(
        env.home().join(".bpm/config.toml"),
        "whitelist = \"*.fq.gz\"\n",
    )
    .unwrap();
    let bad = fail(env.bpm(&["ingest", run42.to_str().unwrap()]));
    assert!(
        bad.err.contains("whitelist must be an array of strings"),
        "{}",
        bad.err
    );
    // Both flags replace the file, so a broken config is not read.
    write(&run42.join("extra.bam"), b"extra");
    write(&run42.join("extra.txt"), b"extra");
    ok(env.bpm(&[
        "ingest",
        run42.to_str().unwrap(),
        "--blacklist",
        "*.txt",
        "--whitelist",
        "*.bam",
    ]));
    let uris = uris(&env);
    assert!(uris.iter().any(|uri| uri == "extra.bam"), "{uris:?}");
    assert!(!uris.iter().any(|uri| uri == "extra.txt"), "{uris:?}");
}

#[test]
#[cfg(unix)]
fn a_closed_stdout_is_sigpipe_not_a_panic() {
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};

    let env = Env::new("sigpipe");
    // The read end is dropped before the child starts, so the first write to
    // stdout finds a pipe with no reader.
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let child = Command::new(env!("CARGO_BIN_EXE_bpm"))
        .current_dir(env.home())
        .env("HOME", env.home())
        .env_remove("BPM_CATALOG")
        .args(["--catalog", env.catalog.to_str().unwrap(), "query", "files"])
        .stdout(writer)
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bpm");
    let output = child.wait_with_output().expect("wait");
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.signal(), Some(13), "{err}");
    assert!(!err.contains("panicked"), "{err}");
}

#[test]
#[cfg(unix)]
fn a_broken_symlink_is_counted_and_the_run_succeeds() {
    use std::os::unix::fs::symlink;
    let env = Env::new("broken-link");
    let one = env.data().join("one");
    write(&one.join("keep.png"), b"png\n");
    symlink(one.join("missing.chain"), one.join("hg19.over.chain")).unwrap();
    let run = env.ingest(&one);
    assert!(
        run.err.contains("bpm: 1 broken symbolic link\n"),
        "{}",
        run.err
    );
    assert!(!run.err.contains("hg19.over.chain"), "{}", run.err);
    assert!(run.out.contains("1 files seen"), "{}", run.out);
    assert!(run.out.contains("0 errors"), "{}", run.out);
    assert_eq!(env.count("files"), 1);
    assert_eq!(env.count("ingest_errors"), 0);

    let two = env.data().join("two");
    write(&two.join("keep.png"), b"png\n");
    symlink(two.join("gone-a"), two.join("a.chain")).unwrap();
    symlink(two.join("gone-b"), two.join("b.chain")).unwrap();
    let run = env.ingest(&two);
    assert!(
        run.err.contains("bpm: 2 broken symbolic links"),
        "{}",
        run.err
    );
    assert!(!run.err.contains("a.chain"), "{}", run.err);
    assert!(!run.err.contains("b.chain"), "{}", run.err);
    assert!(run.out.contains("0 errors"), "{}", run.out);
}

#[test]
#[cfg(unix)]
fn symlinks_follow_the_walk_rules() {
    use std::os::unix::fs::symlink;
    let env = Env::new("links");
    let root = env.data().join("root");
    let outside = env.data().join("outside");
    write(&root.join("real/a.fq"), b"linked bytes\n");
    write(&outside.join("b.fq"), b"outside bytes\n");
    symlink(&outside, root.join("out")).unwrap();
    symlink(root.join("real"), root.join("alias")).unwrap();
    symlink(&root, root.join("real/loop")).unwrap();

    let run = fail(env.bpm(&["ingest", root.to_str().unwrap()]));
    assert!(run.err.contains("outside_root"), "{}", run.err);
    assert!(!run.err.contains("loop"), "{}", run.err);
    // The directory link inside the root is walked once, under one of its two
    // paths; the file there is one file id.
    assert_eq!(env.count("files"), 1);
    assert!(env.files(&[]).iter().all(|file| {
        !file["locations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|location| location["uri"].as_str().unwrap().contains("outside"))
    }));

    // Files under the directory link are recorded under the real path, and
    // either path names them.
    let real = root.join("real/a.fq");
    let alias = root.join("alias/a.fq");
    assert_eq!(env.location(&real)["uri"], real.to_str().unwrap());
    let id = env.id_at(&real);
    env.create(&["program", "--name", "CLL"]);
    ok(env.bpm(&[
        "link",
        "--to",
        "/CLL",
        alias.to_str().unwrap(),
        "--role",
        "data",
    ]));
    ok(env.bpm(&["unlink", "--to", "/CLL", real.to_str().unwrap()]));
    for dir in ["real", "alias"] {
        let scan = env.scan(&root.join(dir));
        assert!(scan.out.contains(": 1 locations"), "{dir}: {}", scan.out);
    }
    ok(env.bpm(&["acknowledge", alias.to_str().unwrap()]));
    assert_eq!(env.id_at(&real), id);

    // A link to a file: the walked path and the target are two locations of
    // one file.
    let flat = env.data().join("flat");
    write(&flat.join("target.fq"), b"target bytes\n");
    symlink(flat.join("target.fq"), flat.join("pointer.fq")).unwrap();
    env.ingest(&flat);
    let file = env.file_at(&flat.join("pointer.fq"));
    assert_eq!(file["locations"].as_array().unwrap().len(), 2);
    assert_eq!(
        env.id_at(&flat.join("target.fq")),
        file["id"].as_str().unwrap()
    );
    // The link's own path names its own location.
    ok(env.bpm(&[
        "delete",
        "--location",
        flat.join("pointer.fq").to_str().unwrap(),
    ]));
    assert_eq!(
        env.file_at(&flat.join("target.fq"))["locations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // A link is filtered by its target too: an allowed name does not let a
    // denied file in, inside the root or outside it.
    let filtered = env.data().join("filtered");
    let hidden = env.data().join("hidden");
    write(&filtered.join(".DS_Store"), b"finder state\n");
    write(&hidden.join("notes.txt"), b"notes\n");
    symlink(filtered.join(".DS_Store"), filtered.join("innocent.fq")).unwrap();
    symlink(hidden.join("notes.txt"), filtered.join("also.fq")).unwrap();
    let run = ok(env.bpm(&["ingest", filtered.to_str().unwrap(), "--blacklist", "*.txt"]));
    assert!(run.out.contains("0 files seen"), "{}", run.out);
}

#[test]
fn ingest_commits_in_batches_and_finishes_large_runs() {
    let env = Env::new("batch");
    let many = env.data().join("many");
    fs::create_dir_all(&many).unwrap();
    for index in 0..2_345 {
        fs::write(
            many.join(format!("f{index:05}.txt")),
            format!("file {index}\n"),
        )
        .unwrap();
    }
    let run = env.ingest(&many);
    assert!(run.out.contains("2345 new files"), "{}", run.out);
    assert_eq!(env.count("files"), 2_345);
    let scan = env.scan(&many);
    assert!(scan.out.contains("2345 ok"), "{}", scan.out);
    assert_eq!(env.count("file_digests"), 2_345);
}

#[test]
fn a_run_whose_process_died_is_marked_incomplete() {
    let env = Env::new("crash");
    let dead = "01900000-0000-7000-8000-000000000001";
    let alive = "01900000-0000-7000-8000-000000000002";
    ok(env.bpm(&[
        "sql",
        "--write",
        &format!(
            "INSERT INTO ingest_runs (id, backend, root_uri, started_at, status, files_seen, files_created)
             VALUES ('{dead}', 'posix', '/x', '2026-01-01T00:00:00.000Z', 'running', 0, 0),
                    ('{alive}', 'posix', '/y', '2026-01-01T00:00:00.000Z', 'running', 0, 0)"
        ),
    ]));
    // A live run holds its lock file; the dead one left a lock file nobody holds.
    let lock_of = |id: &str| PathBuf::from(format!("{}.run-{id}.lock", env.catalog.display()));
    fs::write(lock_of(dead), b"").unwrap();
    let held = fs::File::create(lock_of(alive)).unwrap();
    held.try_lock().unwrap();

    // Any writing command recovers.
    env.create(&["program", "--name", "P"]);
    assert_eq!(
        env.sql("SELECT id, status, finished_at IS NOT NULL AS done FROM ingest_runs ORDER BY id"),
        env.sql(&format!(
            "SELECT '{dead}' AS id, 'incomplete' AS status, 1 AS done
             UNION ALL SELECT '{alive}', 'running', 0"
        ))
    );
    assert!(!lock_of(dead).exists());
    assert!(lock_of(alive).exists());

    drop(held);
    env.create(&["program", "--name", "Q"]);
    assert_eq!(
        env.scalar(&format!(
            "SELECT status FROM ingest_runs WHERE id = '{alive}'"
        )),
        "incomplete"
    );
    // Finished runs leave no lock files behind.
    write(&env.data().join("a.fq"), b"a\n");
    env.ingest(&env.data());
    env.scan(&env.data());
    let leftovers: Vec<_> = fs::read_dir(env.home())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().contains(".run-"))
        .collect();
    assert!(leftovers.is_empty());
}

/// Take a fresh catalog back to version 2 by removing what V003 added.
const BACK_TO_V2: &str = "DROP TABLE link_role_changes;
     DROP TABLE link_runs;
     DROP TABLE link_roles;
     DROP INDEX file_links_run;
     DROP INDEX cases_run;
     DROP INDEX samples_run;
     DROP INDEX raw_data_run;
     DROP INDEX analyses_run;
     ALTER TABLE file_links DROP COLUMN run_id;
     ALTER TABLE cases DROP COLUMN run_id;
     ALTER TABLE samples DROP COLUMN run_id;
     ALTER TABLE raw_data DROP COLUMN run_id;
     ALTER TABLE analyses DROP COLUMN run_id;
     UPDATE catalog_meta SET value = '2' WHERE key = 'schema_version';";

#[test]
fn a_version_1_catalog_migrates_to_the_file_indexes() {
    let env = Env::new("v1");
    {
        let conn = rusqlite::Connection::open(&env.catalog).unwrap();
        conn.execute_batch(BACK_TO_V2).unwrap();
        conn.execute_batch(
            "DROP INDEX files_fingerprint;
             DROP INDEX file_digests_digest;
             UPDATE catalog_meta SET value = '1' WHERE key = 'schema_version';",
        )
        .unwrap();
    }
    // A reader refuses the old version and does not change it.
    let refused = fail(env.bpm(&["query", "files"]));
    assert!(refused.err.contains("older"), "{}", refused.err);
    write(&env.data().join("a.fq"), b"a\n");
    env.ingest(&env.data());
    assert_eq!(
        env.scalar("SELECT value FROM catalog_meta WHERE key = 'schema_version'"),
        "3"
    );
    assert_eq!(
        env.scalar(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'index'
             AND name IN ('files_fingerprint', 'file_digests_digest')"
        ),
        "2"
    );
}

#[test]
fn a_version_2_catalog_keeps_its_links_and_their_roles_become_known() {
    let env = Env::new("v2");
    write(&env.data().join("a.fq"), b"a\n");
    env.ingest(&env.data());
    env.create(&["program", "--name", "CLL"]);
    let id = env.id_at(&env.data().join("a.fq"));
    {
        let conn = rusqlite::Connection::open(&env.catalog).unwrap();
        conn.execute_batch(BACK_TO_V2).unwrap();
        // A role the old `bpm link` accepted, outside the seeded list.
        conn.execute(
            "INSERT INTO file_links (file_id, node_type, node_id, role)
             SELECT ?, 'program', id, 'control' FROM programs",
            [&id],
        )
        .unwrap();
    }
    let refused = fail(env.bpm(&["query", "files"]));
    assert!(refused.err.contains("older"), "{}", refused.err);
    // Any writing command migrates; link is one.
    write(&env.data().join("b.fq"), b"b\n");
    env.ingest(&env.data());
    assert_eq!(
        env.scalar("SELECT value FROM catalog_meta WHERE key = 'schema_version'"),
        "3"
    );
    assert_eq!(
        env.scalar("SELECT COUNT(*) FROM file_links WHERE run_id IS NULL AND role = 'control'"),
        "1"
    );
    // The existing role is known, so it needs no --new-role.
    let b = env.data().join("b.fq");
    ok(env.bpm(&[
        "link",
        "--to",
        "/CLL",
        b.to_str().unwrap(),
        "--role",
        "control",
    ]));
}
