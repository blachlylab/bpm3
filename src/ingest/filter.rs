//! The denylist and the glob whitelist (architecture overview §6, Path filters).

use std::fs;
use std::path::Path;

use globset::{Glob, GlobBuilder, GlobSet, GlobSetBuilder};

use crate::error::Error;

/// Names skipped at any depth unless a run passes `--no-default-denylist`.
pub const BUILT_IN: [&str; 2] = [".DS_Store", "Thumbs.db"];

/// What one run asked for on the command line.
#[derive(Debug, Default, Clone)]
pub struct FilterSpec {
    /// `--denylist`: replaces the global list for this run.
    pub denylist: Option<Vec<String>>,
    pub no_default_denylist: bool,
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

/// The global denylist from `~/.bpm/config.toml`. A missing file is an empty
/// list. A file that is not valid TOML, or a `denylist` that is not an array of
/// strings, is an error rather than a silently ignored setting.
pub fn global_denylist(home: &Path) -> Result<Vec<String>, Error> {
    let path = home.join(".bpm").join("config.toml");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err.into()),
    };
    let table: toml::Table = text
        .parse()
        .map_err(|err| Error::Message(format!("{}: {err}", path.display())))?;
    let Some(value) = table.get("denylist") else {
        return Ok(Vec::new());
    };
    let invalid = || {
        Error::Message(format!(
            "{}: denylist must be an array of strings",
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
    deny: Patterns,
    allow: Patterns,
}

impl PathFilter {
    pub fn new(spec: &FilterSpec, global: Vec<String>) -> Result<Self, Error> {
        let mut deny: Vec<String> = spec.denylist.clone().unwrap_or(global);
        if !spec.no_default_denylist {
            deny.extend(BUILT_IN.iter().map(|name| name.to_string()));
        }
        Ok(Self {
            deny: Patterns::new(deny.iter().map(String::as_str))?,
            allow: Patterns::new(spec.whitelist.iter().map(String::as_str))?,
        })
    }

    /// Whether a path survives the filters. `rel` uses `/` separators and is
    /// relative to the walk or scan root, or is the full URI for a scan of a
    /// whole backend.
    pub fn allows(&self, rel: &str) -> bool {
        if self.deny.matches(rel) {
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
        deny: Option<&[&str]>,
        global: &[&str],
        no_default: bool,
        allow: &[&str],
    ) -> PathFilter {
        PathFilter::new(
            &FilterSpec {
                denylist: deny.map(|list| list.iter().map(|p| p.to_string()).collect()),
                no_default_denylist: no_default,
                whitelist: allow.iter().map(|p| p.to_string()).collect(),
            },
            global.iter().map(|p| p.to_string()).collect(),
        )
        .unwrap()
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
    fn a_run_denylist_replaces_the_global_one_but_keeps_built_ins() {
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
    fn comma_lists_split_and_drop_empty_pieces() {
        assert_eq!(split_patterns("*.txt, *.bak,,"), vec!["*.txt", "*.bak"]);
    }
}
