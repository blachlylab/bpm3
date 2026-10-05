//! The ingest walk: depth-first, sorted by name, symlinks handled per the
//! architecture overview §6.
//!
//! Every directory is walked by its canonical path, so a file reached through
//! a directory link is recorded under the path every later command resolves
//! to. Only a link to a file adds a second, non-canonical location: the link
//! itself.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub enum WalkItem {
    /// A regular file. `rel` is its path relative to the root, which the path
    /// filters match against. For a link to a file, `target_rel` is the
    /// target's: the link is kept only when its target passes the filters too.
    File {
        path: PathBuf,
        rel: String,
        target_rel: Option<String>,
    },
    Error {
        path: PathBuf,
        message: String,
    },
    /// A symlink whose target could not be resolved. It may have been a file
    /// or a directory; the walk cannot tell, and the run does not fail for it.
    BrokenLink,
}

pub struct Walker {
    root: PathBuf,
    stack: Vec<std::vec::IntoIter<PathBuf>>,
    visited: HashSet<PathBuf>,
    pending: VecDeque<WalkItem>,
}

impl Walker {
    /// `root` must already be canonical.
    pub fn new(root: &Path) -> Self {
        let mut walker = Self {
            root: root.to_path_buf(),
            stack: Vec::new(),
            visited: HashSet::from([root.to_path_buf()]),
            pending: VecDeque::new(),
        };
        walker.enter(root);
        walker
    }

    fn enter(&mut self, dir: &Path) {
        match read_sorted(dir) {
            Ok(entries) => self.stack.push(entries.into_iter()),
            Err(err) => self.error(dir, err.to_string()),
        }
    }

    fn error(&mut self, path: &Path, message: String) {
        self.pending.push_back(WalkItem::Error {
            path: path.to_path_buf(),
            message,
        });
    }

    fn broken_link(&mut self) {
        self.pending.push_back(WalkItem::BrokenLink);
    }

    /// The path the filters see: relative to the root, or the final component
    /// for a link target outside it.
    fn rel(&self, path: &Path) -> String {
        match path.strip_prefix(&self.root) {
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        }
    }

    fn file(&mut self, path: PathBuf, target_rel: Option<String>) {
        let rel = self.rel(&path);
        self.pending.push_back(WalkItem::File {
            path,
            rel,
            target_rel,
        });
    }

    fn visit(&mut self, path: PathBuf) {
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(err) => return self.error(&path, err.to_string()),
        };
        if meta.is_symlink() {
            let (target, target_meta) = match fs::canonicalize(&path)
                .and_then(|target| fs::metadata(&target).map(|meta| (target, meta)))
            {
                Ok(found) => found,
                Err(_) => return self.broken_link(),
            };
            if target_meta.is_dir() {
                if !target.starts_with(&self.root) {
                    return self.error(
                        &path,
                        format!(
                            "outside_root: the link points to {}, outside the ingest root",
                            target.display()
                        ),
                    );
                }
                // A directory already walked, by either path, is a cycle or a
                // second route to the same files. Walk it once.
                if self.visited.insert(target.clone()) {
                    self.enter(&target);
                }
            } else if target_meta.is_file() {
                // The link and its target are both candidates, each filtered
                // by its own path.
                let target_rel = self.rel(&target);
                if target != path {
                    self.file(target, None);
                }
                self.file(path, Some(target_rel));
            }
        } else if meta.is_dir() {
            match fs::canonicalize(&path) {
                Ok(canonical) => {
                    if self.visited.insert(canonical.clone()) {
                        self.enter(&canonical);
                    }
                }
                Err(err) => self.error(&path, err.to_string()),
            }
        } else if meta.is_file() {
            self.file(path, None);
        }
    }
}

impl Iterator for Walker {
    type Item = WalkItem;

    fn next(&mut self) -> Option<WalkItem> {
        loop {
            if let Some(item) = self.pending.pop_front() {
                return Some(item);
            }
            let top = self.stack.last_mut()?;
            match top.next() {
                Some(path) => self.visit(path),
                None => {
                    self.stack.pop();
                }
            }
        }
    }
}

fn read_sorted(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut entries = fs::read_dir(dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    entries.sort();
    Ok(entries)
}

/// An absolute, normalized path for a location the operator names, in the form
/// ingest recorded it. A directory is canonicalized in full. For anything else
/// the parent is canonicalized and the final name kept, so a link to a file
/// names that link's own location. The longest part of the parent that exists
/// is canonicalized, so a path that has gone away still maps through symlinked
/// parents such as macOS's `/var` → `/private/var`, and the rest is normalized
/// lexically.
pub fn location_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if fs::metadata(&absolute).is_ok_and(|meta| meta.is_dir()) {
        return fs::canonicalize(&absolute);
    }
    match (absolute.parent(), absolute.file_name()) {
        (Some(parent), Some(name)) => {
            let mut out = existing_prefix(parent)?;
            out.push(name);
            Ok(out)
        }
        _ => existing_prefix(&absolute),
    }
}

fn existing_prefix(absolute: &Path) -> io::Result<PathBuf> {
    let absolute = absolute.to_path_buf();
    let mut existing = absolute.clone();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match fs::canonicalize(&existing) {
            Ok(canonical) => {
                let mut out = canonical;
                for part in rest.iter().rev() {
                    push_lexical(&mut out, part);
                }
                return Ok(out);
            }
            Err(_) => {
                let Some(name) = existing.file_name().map(|name| name.to_os_string()) else {
                    return Ok(lexical(&absolute));
                };
                rest.push(name);
                if !existing.pop() {
                    return Ok(lexical(&absolute));
                }
            }
        }
    }
}

fn push_lexical(out: &mut PathBuf, part: &std::ffi::OsStr) {
    match part.to_str() {
        Some(".") => {}
        Some("..") => {
            out.pop();
        }
        _ => out.push(part),
    }
}

fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}
