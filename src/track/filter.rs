//! The blacklist and the glob whitelist (architecture overview §6, Path filters).

use std::fs;
use std::path::Path;

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

use crate::error::Error;

/// Names skipped at any depth unless a run passes `--no-default-blacklist`.
pub const BUILT_IN: [&str; 2] = [".DS_Store", "Thumbs.db"];

/// What one run asked for on the command line.
#[derive(Debug, Default, Clone)]
pub struct FilterSpec {
    /// `--blacklist`: replaces the global list for this run.
    pub blacklist: Option<Vec<String>>,
    pub no_default_blacklist: bool,
    /// `--whitelist`: replaces the global list for this run.
    /// `None` uses the config list. `Some` replaces it, including an empty list.
    pub whitelist: Option<Vec<String>>,
}

/// The two lists from `~/.bpm/config.toml`. A missing file or a missing key is empty.
#[derive(Debug, Default, Clone)]
pub struct GlobalLists {
    pub blacklist: Vec<String>,
    pub whitelist: Vec<String>,
}

/// Split a comma-separated flag value. Empty pieces are dropped.
pub fn split_patterns(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|piece| !piece.is_empty())
        .map(String::from)
        .collect()
}

/// The global lists from `~/.bpm/config.toml`. A missing file is two empty
/// lists. A file that is not valid TOML, or a `blacklist` or `whitelist` that
/// is not an array of strings, is an error rather than a silently ignored setting.
pub fn global_lists(home: &Path) -> Result<GlobalLists, Error> {
    let path = home.join(".bpm").join("config.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(GlobalLists::default()),
        Err(err) => return Err(err.into()),
    };
    let table: toml::Table = text
        .parse()
        .map_err(|err| Error::Message(format!("{}: {err}", path.display())))?;
    Ok(GlobalLists {
        blacklist: string_array(&table, "blacklist", &path)?,
        whitelist: string_array(&table, "whitelist", &path)?,
    })
}

fn string_array(table: &toml::Table, key: &str, path: &Path) -> Result<Vec<String>, Error> {
    let Some(value) = table.get(key) else {
        return Ok(Vec::new());
    };
    let invalid = || {
        Error::Message(format!(
            "{}: {key} must be an array of strings",
            path.display()
        ))
    };
    value
        .as_array()
        .ok_or_else(invalid)?
        .iter()
        .map(|item| item.as_str().map(String::from).ok_or_else(invalid))
        .collect()
}

/// One side of the filter. A pattern without `/` matches the final path
/// component. A pattern with `/` matches the whole relative path. `*` does not
/// cross `/`; `**` does.
struct Patterns {
    names: GlobSet,
    paths: GlobSet,
    empty: bool,
}

impl Patterns {
    fn new<'a>(patterns: impl IntoIterator<Item = &'a str>) -> Result<Self, Error> {
        let mut names = GlobSetBuilder::new();
        let mut paths = GlobSetBuilder::new();
        let mut empty = true;
        for pattern in patterns {
            empty = false;
            if pattern.contains('/') {
                paths.add(glob(pattern)?);
            } else {
                names.add(glob(pattern)?);
            }
        }
        Ok(Self {
            names: names.build().map_err(pattern_error)?,
            paths: paths.build().map_err(pattern_error)?,
            empty,
        })
    }

    fn matches(&self, rel: &str) -> bool {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        self.names.is_match(name) || self.paths.is_match(rel)
    }
}

pub struct PathFilter {
    blacklist: Patterns,
    allow: Patterns,
    /// Patterns in effect, including the built-in names. For the kickoff line.
    blacklist_patterns: Vec<String>,
    whitelist_patterns: Vec<String>,
}

impl PathFilter {
    pub fn new(spec: &FilterSpec, global: GlobalLists) -> Result<Self, Error> {
        let mut blacklist: Vec<String> = spec.blacklist.clone().unwrap_or(global.blacklist);
        if !spec.no_default_blacklist {
            blacklist.extend(BUILT_IN.iter().map(|name| name.to_string()));
        }
        let whitelist = spec.whitelist.clone().unwrap_or(global.whitelist);
        let blacklist_globs = Patterns::new(blacklist.iter().map(String::as_str))?;
        let allow = Patterns::new(whitelist.iter().map(String::as_str))?;
        Ok(Self {
            blacklist: blacklist_globs,
            allow,
            blacklist_patterns: blacklist,
            whitelist_patterns: whitelist,
        })
    }

    pub(crate) fn blacklist_patterns(&self) -> &[String] {
        &self.blacklist_patterns
    }

    pub(crate) fn whitelist_patterns(&self) -> &[String] {
        &self.whitelist_patterns
    }

    /// Whether a path survives the filters. `rel` uses `/` separators and is
    /// relative to the walk or scan root, or is the full URI for a scan of a
    /// whole backend.
    pub fn allows(&self, rel: &str) -> bool {
        if self.blacklist.matches(rel) {
            return false;
        }
        self.allow.empty || self.allow.matches(rel)
    }
}

fn glob(pattern: &str) -> Result<Glob, Error> {
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map_err(pattern_error)
}

fn pattern_error(err: globset::Error) -> Error {
    Error::Message(format!("invalid pattern: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(
        blacklist: Option<&[&str]>,
        global: &[&str],
        no_default: bool,
        allow: &[&str],
    ) -> PathFilter {
        PathFilter::new(
            &FilterSpec {
                blacklist: blacklist.map(|list| list.iter().map(|p| p.to_string()).collect()),
                no_default_blacklist: no_default,
                whitelist: Some(allow.iter().map(|p| p.to_string()).collect()),
            },
            GlobalLists {
                blacklist: global.iter().map(|p| p.to_string()).collect(),
                whitelist: Vec::new(),
            },
        )
        .unwrap()
    }

    fn lists(home_text: Option<&str>) -> Result<GlobalLists, Error> {
        let home = std::env::temp_dir().join(format!(
            "bpm3-filter-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let result = (|| {
            if let Some(text) = home_text {
                fs::create_dir_all(home.join(".bpm"))?;
                fs::write(home.join(".bpm/config.toml"), text)?;
            }
            global_lists(&home)
        })();
        let _ = fs::remove_dir_all(&home);
        result
    }

    #[test]
    fn built_in_names_apply_at_any_depth_unless_turned_off() {
        let default = filter(None, &[], false, &[]);
        assert!(!default.allows(".DS_Store"));
        assert!(!default.allows("a/b/Thumbs.db"));
        assert!(default.allows("a/b/S1_R1.fq.gz"));
        let off = filter(None, &[], true, &[]);
        assert!(off.allows("a/.DS_Store"));
    }

    #[test]
    fn a_run_blacklist_replaces_the_global_one_but_keeps_built_ins() {
        let global = filter(None, &["*.txt", "scratch/**"], false, &[]);
        assert!(!global.allows("notes.txt"));
        assert!(!global.allows("deep/notes.txt"));
        assert!(!global.allows("scratch/x/y.fq"));
        assert!(global.allows("keep/scratch/y.fq"));
        let run = filter(Some(&["*.bak"]), &["*.txt"], false, &[]);
        assert!(run.allows("notes.txt"));
        assert!(!run.allows("old.bak"));
        assert!(!run.allows(".DS_Store"));
    }

    #[test]
    fn the_whitelist_needs_a_match_and_star_does_not_cross_slash() {
        let only = filter(None, &[], false, &["*.fq.gz", "bams/*.bam"]);
        assert!(only.allows("S1_R1.fq.gz"));
        assert!(only.allows("lane1/S1_R1.fq.gz"));
        assert!(only.allows("bams/S1.bam"));
        assert!(!only.allows("bams/sub/S1.bam"));
        assert!(!only.allows("S1.bam"));
        let deep = filter(None, &[], false, &["bams/**"]);
        assert!(deep.allows("bams/sub/S1.bam"));
        // Denied wins over allowed.
        let both = filter(Some(&["*.tmp.fq.gz"]), &[], false, &["*.fq.gz"]);
        assert!(!both.allows("x.tmp.fq.gz"));
    }

    #[test]
    fn a_run_whitelist_replaces_the_global_one() {
        let global = PathFilter::new(
            &FilterSpec {
                whitelist: None,
                ..FilterSpec::default()
            },
            GlobalLists {
                blacklist: Vec::new(),
                whitelist: vec!["*.fq.gz".into()],
            },
        )
        .unwrap();
        assert!(global.allows("lane1/S1.fq.gz"));
        assert!(!global.allows("S1.bam"));
        assert!(!global.allows(".DS_Store"));

        let run = PathFilter::new(
            &FilterSpec {
                whitelist: Some(vec!["*.bam".into()]),
                ..FilterSpec::default()
            },
            GlobalLists {
                blacklist: Vec::new(),
                whitelist: vec!["*.fq.gz".into()],
            },
        )
        .unwrap();
        assert!(run.allows("S1.bam"));
        assert!(!run.allows("S1.fq.gz"));

        let cleared = PathFilter::new(
            &FilterSpec {
                whitelist: Some(Vec::new()),
                no_default_blacklist: true,
                ..FilterSpec::default()
            },
            GlobalLists {
                blacklist: Vec::new(),
                whitelist: vec!["*.fq.gz".into()],
            },
        )
        .unwrap();
        assert!(cleared.allows("notes.txt"));
        assert!(cleared.allows(".DS_Store"));
    }

    #[test]
    fn config_toml_reads_both_lists_and_rejects_a_string() {
        let missing = lists(None).unwrap();
        assert!(missing.blacklist.is_empty());
        assert!(missing.whitelist.is_empty());

        let both = lists(Some(
            "blacklist = [\"*.txt\"]\nwhitelist = [\"*.fq.gz\", \"bams/**\"]\n",
        ))
        .unwrap();
        assert_eq!(both.blacklist, vec!["*.txt"]);
        assert_eq!(both.whitelist, vec!["*.fq.gz", "bams/**"]);

        let only_blacklist = lists(Some("blacklist = [\"*.bak\"]\n")).unwrap();
        assert_eq!(only_blacklist.whitelist, Vec::<String>::new());

        let bad_whitelist = lists(Some("whitelist = \"*.fq.gz\"\n")).unwrap_err();
        assert!(
            bad_whitelist
                .to_string()
                .contains("whitelist must be an array of strings"),
            "{bad_whitelist}"
        );
        let bad_blacklist = lists(Some("blacklist = \"*.txt\"\n")).unwrap_err();
        assert!(
            bad_blacklist
                .to_string()
                .contains("blacklist must be an array of strings"),
            "{bad_blacklist}"
        );
    }

    #[test]
    fn comma_lists_split_and_drop_empty_pieces() {
        assert_eq!(split_patterns("*.txt, *.bak,,"), vec!["*.txt", "*.bak"]);
    }
}
