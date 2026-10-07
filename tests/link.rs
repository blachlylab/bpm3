//! Bulk link, unlink, and undo: the four scenarios in the link guide, the
//! checks around them, and undo. Each test uses its own HOME, catalog, and data
//! directory. A test process has no terminal, so a run that creates entities
//! needs `--yes`.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "bpm3-link-{label}-{}-{}",
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

    fn dir(&self) -> String {
        self.data().to_str().unwrap().to_string()
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

    /// Write files under the data directory and ingest them.
    fn files(&self, names: &[&str]) {
        for name in names {
            let path = self.data().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, format!("bytes of {name}\n")).unwrap();
        }
        ok(self.bpm(&["ingest", &self.dir()]));
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

    /// Entity counts: (case, sample, raw_data, analysis).
    fn tree(&self) -> (i64, i64, i64, i64) {
        (
            self.count("cases"),
            self.count("samples"),
            self.count("raw_data"),
            self.count("analyses"),
        )
    }

    fn under(&self, address: &str) -> Vec<Value> {
        let out = ok(self.bpm(&["query", "files", "--under", address, "--format", "json"])).out;
        serde_json::from_str::<Value>(&out)
            .unwrap()
            .as_array()
            .unwrap()
            .clone()
    }

    fn create(&self, args: &[&str]) -> String {
        let mut full = vec!["create"];
        full.extend_from_slice(args);
        ok(self.bpm(&full)).out.trim().to_string()
    }

    /// The run id from `link run <id>: …`.
    fn run_id(run: &Run) -> String {
        run.out
            .lines()
            .find_map(|line| line.strip_prefix("link run ")?.split(':').next())
            .unwrap_or_else(|| panic!("no run id in\n{}", run.out))
            .to_string()
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

const BAM_TO: &str = "/P/J/case[subject_id:{1}]?/sample?/raw_data?/analysis[pipeline:bwa]?";
const BAM_MATCH: &str = "([A-Za-z0-9]+)_aln\\.ba[mi]";

#[test]
fn scenario_1_bams_build_the_tree_and_a_rerun_writes_nothing() {
    let env = Env::new("s1");
    let dir = env.dir();
    env.files(&[
        "S1_aln.bam",
        "S1_aln.bai",
        "S2_aln.bam",
        "S2_aln.bai",
        "README.txt",
    ]);
    let to = [
        "link", "--to", BAM_TO, "--match", BAM_MATCH, "--role", "data",
    ];

    // Dry run: the plan, and nothing written.
    let mut args = to.to_vec();
    args.extend(["-n", &dir]);
    let plan = ok(env.bpm(&args));
    assert!(plan.out.contains("1 not matched"), "{}", plan.out);
    assert!(
        plan.out
            .contains("create 2 case, 2 sample, 2 raw_data, 2 analysis"),
        "{}",
        plan.out
    );
    assert_eq!(env.tree(), (0, 0, 0, 0));

    // Creating entities needs a terminal or --yes.
    let mut args = to.to_vec();
    args.push(&dir);
    let refused = fail(env.bpm(&args));
    assert!(refused.err.contains("--yes"), "{}", refused.err);
    assert_eq!(env.tree(), (0, 0, 0, 0));

    let mut args = to.to_vec();
    args.extend(["--yes", &dir]);
    ok(env.bpm(&args));
    assert_eq!(env.tree(), (2, 2, 2, 2));
    // The BAM and its index share the analysis; created nodes carry their pairs.
    let s1 = env.under("/P/J/subject_id:S1");
    assert_eq!(s1.len(), 2);
    assert_eq!(s1[0]["links"][0]["node_type"], "analysis");
    assert_eq!(
        env.scalar("SELECT COUNT(*) FROM entity_metadata WHERE key = 'pipeline' AND value = 'bwa'"),
        "2"
    );

    // A rerun finds every link already there.
    let mut args = to.to_vec();
    args.extend(["--yes", &dir]);
    let again = ok(env.bpm(&args));
    assert!(again.out.contains("already linked 4"), "{}", again.out);
    assert!(again.out.contains("nothing to write"), "{}", again.out);
    assert_eq!(env.tree(), (2, 2, 2, 2));
}

#[test]
fn scenario_2_existing_cases_must_all_be_found() {
    let env = Env::new("s2");
    let dir = env.dir();
    for subject in ["S1", "S2", "S9"] {
        let case = env.create(&["case", "--parent", "/P/J"]);
        ok(env.bpm(&["meta", "set", &case, "subject_id", subject]));
    }
    env.files(&["S1_aln.bam", "S2_aln.bam", "S3_aln.bam"]);
    let to = "/P/J/case[subject_id:{1}]/sample?/raw_data?/analysis+";

    // S3 has no case: the whole run fails and writes nothing.
    let missing = fail(env.bpm(&[
        "link", "--to", to, "--match", BAM_MATCH, "--role", "data", "--yes", &dir,
    ]));
    assert!(
        missing
            .err
            .contains("nothing matches /P/J/case[subject_id:S3]"),
        "{}",
        missing.err
    );
    assert_eq!(env.tree(), (3, 0, 0, 0));

    fs::remove_file(env.data().join("S3_aln.bam")).unwrap();
    let done = ok(env.bpm(&[
        "link",
        "--to",
        to,
        "--match",
        BAM_MATCH,
        "--role",
        "data",
        "--yes",
        env.data().join("S1_aln.bam").to_str().unwrap(),
        env.data().join("S2_aln.bam").to_str().unwrap(),
    ]));
    // Coverage: S9 got nothing.
    assert!(
        done.out
            .contains("note: 1 existing case under the same parents got no files"),
        "{}",
        done.out
    );
    assert_eq!(env.tree(), (3, 2, 2, 2));
}

#[test]
fn scenario_3_one_raw_data_per_fastq_and_expect_catches_a_missing_mate() {
    let env = Env::new("s3");
    let dir = env.dir();
    env.files(&["S1_L1_R1.fq.gz", "S1_L1_R2.fq.gz", "S2_L1_R1.fq.gz"]);
    let args = |extra: &[&str]| {
        let mut args = vec![
            "link",
            "--to",
            "/P/J/case[subject_id:{1}]?/sample?/raw_data[read:{2}]+",
            "--match",
            "([A-Za-z0-9]+)_L1_(R[12])\\.fq\\.gz",
            "--role",
            "data",
            "--expect",
            "sample=2",
            "--expect",
            "raw_data=1",
            "--yes",
        ];
        args.extend_from_slice(extra);
        args.iter().map(|s| s.to_string()).collect::<Vec<_>>()
    };
    let run = |extra: &[&str]| {
        let owned = args(extra);
        env.bpm(&owned.iter().map(String::as_str).collect::<Vec<_>>())
    };

    let short = fail(run(&[&dir]));
    assert!(
        short
            .err
            .contains("--expect sample=2: /P/J/case[subject_id:S2]?/sample? gets 1 file"),
        "{}",
        short.err
    );
    assert_eq!(env.tree(), (0, 0, 0, 0));

    env.files(&["S2_L1_R2.fq.gz"]);
    ok(run(&[&dir]));
    assert_eq!(env.tree(), (2, 2, 4, 0));
    assert_eq!(
        env.scalar(
            "SELECT COUNT(*) FROM entity_metadata WHERE node_type = 'raw_data' AND key = 'read'"
        ),
        "4"
    );

    // Two lanes that the pattern does not tell apart collapse onto one node,
    // and --expect raw_data=1 says so.
    let env = Env::new("s3-lanes");
    let dir = env.dir();
    env.files(&["S1_L1_R1.fq.gz", "S1_L2_R1.fq.gz"]);
    let collapsed = fail(env.bpm(&[
        "link",
        "--to",
        "/P/J/case[subject_id:{1}]?/sample?/raw_data[read:{2}]+",
        "--match",
        "([A-Za-z0-9]+)_L[12]_(R[12])\\.fq\\.gz",
        "--role",
        "data",
        "--expect",
        "raw_data=1",
        "--yes",
        &dir,
    ]));
    assert!(collapsed.err.contains("gets 2 files"), "{}", collapsed.err);
}

#[test]
fn scenario_4_one_file_to_an_existing_case() {
    let env = Env::new("s4");
    let case = env.create(&["case", "--parent", "/P/J"]);
    ok(env.bpm(&["meta", "set", &case, "subject_id", "CLL-001"]));
    env.files(&["consent.xlsx"]);
    let file = env.data().join("consent.xlsx");
    ok(env.bpm(&[
        "link",
        "--to",
        "/P/J/case[subject_id:CLL-001]",
        "--role",
        "document",
        file.to_str().unwrap(),
    ]));
    assert_eq!(env.under(&case).len(), 1);
    // The address form from before works the same way.
    ok(env.bpm(&[
        "unlink",
        "--to",
        "/P/J/subject_id:CLL-001",
        file.to_str().unwrap(),
    ]));
    assert!(env.under(&case).is_empty());
}

#[test]
fn two_assays_in_two_runs_share_one_sample() {
    let env = Env::new("assays");
    env.files(&["wes/S1.fq.gz", "wgs/S1.fq.gz"]);
    for assay in ["WES", "WGS"] {
        let to = format!("/P/J/case[subject_id:{{1}}]?/sample?/raw_data[assay:{assay}]+");
        let dir = env.data().join(assay.to_lowercase());
        ok(env.bpm(&[
            "link",
            "--to",
            &to,
            "--match",
            "(S[0-9]+)\\.fq\\.gz",
            "--role",
            "data",
            "--yes",
            dir.to_str().unwrap(),
        ]));
    }
    assert_eq!(env.tree(), (1, 1, 2, 0));

    // A second sample makes sample? ambiguous instead of picking one.
    let case = env.scalar("SELECT id FROM cases");
    env.create(&["sample", "--parent", &case]);
    env.files(&["rna/S1.fq.gz"]);
    let ambiguous = fail(env.bpm(&[
        "link",
        "--to",
        "/P/J/case[subject_id:{1}]?/sample?/raw_data[assay:RNA-seq]+",
        "--match",
        "(S[0-9]+)\\.fq\\.gz",
        "--role",
        "data",
        "--yes",
        env.data().join("rna").to_str().unwrap(),
    ]));
    assert!(
        ambiguous.err.contains("matches 2 entities"),
        "{}",
        ambiguous.err
    );
}

#[test]
fn match_path_captures_directories() {
    let env = Env::new("path");
    let dir = env.dir();
    env.files(&["S1/aln/x.bam", "S2/aln/y.bam"]);
    ok(env.bpm(&[
        "link",
        "--to",
        "/P/J/case[subject_id:{subj}]?",
        "--match-path",
        "(?<subj>[^/]+)/aln/[^/]+\\.bam",
        "--role",
        "data",
        "--yes",
        &dir,
    ]));
    assert_eq!(env.under("/P/J/subject_id:S2").len(), 1);
    // --match sees only the name, so the directory is out of its reach.
    let name_only = fail(env.bpm(&[
        "link",
        "--to",
        "/P/J/case[subject_id:{subj}]?",
        "--match",
        "(?<subj>[^/]+)/aln/[^/]+\\.bam",
        "--role",
        "data",
        "--yes",
        &dir,
    ]));
    assert!(
        name_only.err.contains("no files to link"),
        "{}",
        name_only.err
    );
}

#[test]
fn a_mapping_table_supplies_placeholders() {
    let env = Env::new("table");
    let dir = env.dir();
    env.files(&["BC01.fq.gz", "BC02.fq.gz", "BC03.fq.gz"]);
    let table = env.data().join("../samples.tsv");
    fs::write(
        &table,
        "barcode\tsubject\ttissue\nBC01\tS1\ttumor\nBC02\tS1\tnormal\nBC04\tS4\ttumor\n",
    )
    .unwrap();
    let base = [
        "link",
        "--to",
        "/P/J/case[subject_id:{subject}]?/sample[tissue:{tissue}]?/raw_data+",
        "--match",
        "(BC[0-9]+)\\.fq\\.gz",
        "--table",
        table.to_str().unwrap(),
        "--join",
        "barcode={1}",
        "--role",
        "data",
        "--yes",
    ];
    // BC03 has no row: the run fails.
    let mut args = base.to_vec();
    args.push(&dir);
    let norow = fail(env.bpm(&args));
    assert!(
        norow.err.contains("no table row has barcode \"BC03\""),
        "{}",
        norow.err
    );

    let mut args = base.to_vec();
    let bc01 = env.data().join("BC01.fq.gz");
    let bc02 = env.data().join("BC02.fq.gz");
    args.extend([bc01.to_str().unwrap(), bc02.to_str().unwrap()]);
    let done = ok(env.bpm(&args));
    assert!(
        done.out.contains("1 of 3 table rows, matched no file"),
        "{}",
        done.out
    );
    assert_eq!(env.tree(), (1, 2, 2, 0));

    // --strict turns an unused row into an error.
    let mut args = base.to_vec();
    args.extend(["--strict", bc01.to_str().unwrap()]);
    fail(env.bpm(&args));
}

#[test]
fn roles_are_checked_against_the_catalog_list() {
    let env = Env::new("roles");
    env.files(&["a.txt"]);
    let file = env.data().join("a.txt");
    let file = file.to_str().unwrap();
    let upper = fail(env.bpm(&["link", "--to", "/P/J", "--role", "Data", file]));
    assert!(upper.err.contains("lowercase"), "{}", upper.err);
    let new = fail(env.bpm(&["link", "--to", "/P/J", "--role", "samplesheet", file]));
    assert!(new.err.contains("--new-role"), "{}", new.err);
    ok(env.bpm(&[
        "link",
        "--to",
        "/P/J",
        "--role",
        "samplesheet",
        "--new-role",
        file,
    ]));
    // Known now; a different role on the same link needs --set-role.
    let other = fail(env.bpm(&["link", "--to", "/P/J", "--role", "document", file]));
    assert!(other.err.contains("--set-role"), "{}", other.err);
    let changed = ok(env.bpm(&[
        "link",
        "--to",
        "/P/J",
        "--role",
        "document",
        "--set-role",
        file,
    ]));
    assert_eq!(env.under("/P/J")[0]["links"][0]["role"], "document");
    let summary = ok(env.bpm(&["query", "summary"])).out;
    assert!(summary.contains("role"), "{summary}");

    // Undo puts the old role back.
    ok(env.bpm(&["undo", &Env::run_id(&changed)]));
    assert_eq!(env.under("/P/J")[0]["links"][0]["role"], "samplesheet");
}

#[test]
fn undo_removes_a_run_and_refuses_while_later_work_depends_on_it() {
    let env = Env::new("undo");
    let dir = env.dir();
    env.files(&["S1_aln.bam", "S1_aln.bai"]);
    let first = ok(env.bpm(&[
        "link",
        "--to",
        BAM_TO,
        "--match",
        "([A-Za-z0-9]+)_aln\\.bam",
        "--role",
        "data",
        "--yes",
        &dir,
    ]));
    let second = ok(env.bpm(&[
        "link",
        "--to",
        "/P/J/case[subject_id:{1}]/sample/raw_data/analysis[pipeline:bwa]",
        "--match",
        "([A-Za-z0-9]+)_aln\\.bai",
        "--role",
        "index",
        &dir,
    ]));
    let (first, second) = (Env::run_id(&first), Env::run_id(&second));

    let blocked = fail(env.bpm(&["undo", &first, "--yes"]));
    assert!(blocked.err.contains(&second), "{}", blocked.err);
    assert!(blocked.err.contains("undo it first"), "{}", blocked.err);
    assert_eq!(env.tree(), (1, 1, 1, 1));

    // Deleting entities needs a terminal or --yes; the second run deletes none.
    ok(env.bpm(&["undo", &second]));
    fail(env.bpm(&["undo", &first]));
    let preview = ok(env.bpm(&["undo", &first, "-n"]));
    assert!(
        preview.out.contains("remove 1 links and 4 entities"),
        "{}",
        preview.out
    );
    ok(env.bpm(&["undo", &first, "--yes"]));
    assert_eq!(env.tree(), (0, 0, 0, 0));
    assert_eq!(env.count("file_links"), 0);
    assert_eq!(env.count("files"), 2);
    let twice = fail(env.bpm(&["undo", &first, "--yes"]));
    assert!(twice.err.contains("was undone"), "{}", twice.err);
    let list = ok(env.bpm(&["undo", "--list"])).out;
    assert!(list.contains(&first) && list.contains(&second), "{list}");

    // Metadata set on a created entity after the run blocks its undo.
    let run = ok(env.bpm(&[
        "link",
        "--to",
        BAM_TO,
        "--match",
        "([A-Za-z0-9]+)_aln\\.bam",
        "--role",
        "data",
        "--yes",
        &dir,
    ]));
    let case = env.scalar("SELECT id FROM cases");
    ok(env.bpm(&["meta", "set", &case, "consent", "GRU"]));
    let edited = fail(env.bpm(&["undo", &Env::run_id(&run), "--yes"]));
    assert!(
        edited.err.contains("metadata set after this run: consent"),
        "{}",
        edited.err
    );
}

#[test]
fn a_rerun_with_new_steps_skips_files_already_linked_under_the_anchor() {
    let env = Env::new("skip");
    let dir = env.dir();
    env.files(&["S1_R1.fq.gz"]);
    let base = [
        "link",
        "--to",
        "/P/J/case[subject_id:{1}]?/sample?/raw_data+",
        "--match",
        "(S[0-9]+)_R1\\.fq\\.gz",
        "--role",
        "data",
        "--yes",
    ];
    let mut args = base.to_vec();
    args.push(&dir);
    ok(env.bpm(&args));
    let again = ok(env.bpm(&args));
    assert!(again.out.contains("skip 1"), "{}", again.out);
    assert_eq!(env.tree(), (1, 1, 1, 0));
    let mut args = base.to_vec();
    args.extend(["--relink", &dir]);
    ok(env.bpm(&args));
    assert_eq!(env.tree(), (1, 1, 2, 0));
}

#[test]
fn bulk_unlink_uses_the_same_template_and_never_creates() {
    let env = Env::new("unlink");
    let dir = env.dir();
    env.files(&["S1_aln.bam", "S2_aln.bam"]);
    ok(env.bpm(&[
        "link", "--to", BAM_TO, "--match", BAM_MATCH, "--role", "data", "--yes", &dir,
    ]));
    let creating = fail(env.bpm(&["unlink", "--to", BAM_TO, "--match", BAM_MATCH, &dir]));
    assert!(
        creating.err.contains("unlink never creates"),
        "{}",
        creating.err
    );
    ok(env.bpm(&[
        "unlink",
        "--to",
        "/P/J/case[subject_id:{1}]/sample/raw_data/analysis",
        "--match",
        BAM_MATCH,
        &dir,
    ]));
    assert_eq!(env.count("file_links"), 0);
    assert_eq!(env.tree(), (2, 2, 2, 2));
}

#[test]
fn template_and_operand_errors_name_the_fix() {
    let env = Env::new("errors");
    let dir = env.dir();
    env.files(&["S1_aln.bam"]);
    let skipped = fail(env.bpm(&[
        "link",
        "--to",
        "/P/J/case[subject_id:{1}]?/analysis+",
        "--match",
        BAM_MATCH,
        "--role",
        "data",
        &dir,
    ]));
    assert!(
        skipped
            .err
            .contains("/P/J/case[subject_id:{1}]?/sample?/raw_data?/analysis+"),
        "{}",
        skipped.err
    );
    let undefined = fail(env.bpm(&[
        "link",
        "--to",
        "/P/J/case[subject_id:{1}]?",
        "--role",
        "data",
        &dir,
    ]));
    assert!(undefined.err.contains("needs --match"), "{}", undefined.err);
    let absent = fail(env.bpm(&[
        "link",
        "--to",
        "/P/J",
        "--role",
        "data",
        env.data().join("nowhere").to_str().unwrap(),
    ]));
    assert!(absent.err.contains("bpm ingest"), "{}", absent.err);
    let s3 = fail(env.bpm(&["link", "--to", "/P/J", "--role", "data", "s3://bucket/key"]));
    assert!(s3.err.contains("not part of this milestone"), "{}", s3.err);
    let expect = fail(env.bpm(&[
        "link", "--to", "/P/J", "--role", "data", "--expect", "sample=2", &dir,
    ]));
    assert!(expect.err.contains("no sample step"), "{}", expect.err);
}
