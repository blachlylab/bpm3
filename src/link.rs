//! `bpm link` templates: the `--to` grammar, placeholders, `--match`, the
//! mapping table, and what each file renders to. Nothing here reads the
//! catalog; [`crate::catalog::Catalog::plan_links`] resolves what this renders.
//!
//! A template is an anchor followed by typed steps:
//!
//! ```text
//! /CLL/WES/case[subject_id:{1}]?/sample?/raw_data[read:{2}]+
//! ```
//!
//! The anchor is any address `bpm` already accepts. A typed step is one level
//! down: a node type, optional `[key:value, …]` pairs, and a mode. No mode means
//! the node must exist; `?` uses the one match or creates it; `+` creates one
//! node per distinct key within the run.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;

use regex::Regex;

use crate::error::Error;
use crate::model::{NodeType, Selector, validate_meta_token};

/// What a typed step does when it looks for its node under the parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    /// Exactly one match, or the file fails.
    Exist,
    /// `?`: the one match; none creates it; more than one fails.
    Ensure,
    /// `+`: a new node, one per distinct key within the run.
    New,
}

impl Mode {
    fn suffix(self) -> &'static str {
        match self {
            Self::Exist => "",
            Self::Ensure => "?",
            Self::New => "+",
        }
    }
}

/// One piece of template text: literal characters and `{name}` placeholders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text(Vec<Piece>);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Lit(String),
    Var(String),
}

impl Text {
    /// `{name}` is a placeholder; `{{` and `}}` are literal braces.
    pub fn parse(raw: &str) -> Result<Self, Error> {
        let mut pieces = Vec::new();
        let mut lit = String::new();
        let mut chars = raw.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '{' if chars.peek() == Some(&'{') => {
                    chars.next();
                    lit.push('{');
                }
                '}' if chars.peek() == Some(&'}') => {
                    chars.next();
                    lit.push('}');
                }
                '{' => {
                    let mut name = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(c) if c.is_ascii_alphanumeric() || c == '_' => name.push(c),
                            _ => {
                                return Err(template_error(format!(
                                    "{raw}: a placeholder is {{name}}, with letters, digits, and _; write {{{{ or }}}} for a literal brace"
                                )));
                            }
                        }
                    }
                    if name.is_empty() {
                        return Err(template_error(format!("{raw}: empty placeholder {{}}")));
                    }
                    if !lit.is_empty() {
                        pieces.push(Piece::Lit(std::mem::take(&mut lit)));
                    }
                    pieces.push(Piece::Var(name));
                }
                '}' => {
                    return Err(template_error(format!(
                        "{raw}: unmatched }}; write }}}} for a literal brace"
                    )));
                }
                c => lit.push(c),
            }
        }
        if !lit.is_empty() {
            pieces.push(Piece::Lit(lit));
        }
        Ok(Self(pieces))
    }

    fn vars(&self) -> impl Iterator<Item = &str> {
        self.0.iter().filter_map(|piece| match piece {
            Piece::Var(name) => Some(name.as_str()),
            Piece::Lit(_) => None,
        })
    }

    fn contains_literal(&self, c: char) -> bool {
        self.0
            .iter()
            .any(|piece| matches!(piece, Piece::Lit(text) if text.contains(c)))
    }

    /// Fill in the placeholders. A value with `/` or `:` would change the shape
    /// of a path or a pair, so it is refused here, wherever it is used.
    fn render(&self, vars: &Vars) -> Result<String, String> {
        let mut out = String::new();
        for piece in &self.0 {
            match piece {
                Piece::Lit(text) => out.push_str(text),
                Piece::Var(name) => {
                    let value = vars.get(name)?;
                    if value.contains('/') || value.contains(':') {
                        return Err(format!("{{{name}}} is {value:?}, which contains / or :"));
                    }
                    out.push_str(value);
                }
            }
        }
        Ok(out)
    }
}

impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for piece in &self.0 {
            match piece {
                Piece::Lit(text) => write!(f, "{}", text.replace('{', "{{").replace('}', "}}"))?,
                Piece::Var(name) => write!(f, "{{{name}}}")?,
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StepTemplate {
    node_type: NodeType,
    /// `(key, value)`; either side may be absent on a step that must exist.
    pairs: Vec<(Option<Text>, Option<Text>)>,
    mode: Mode,
}

/// A parsed `--to`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    source: String,
    /// Anchor segments, rendered and joined with `/` into an address.
    anchor: Vec<Text>,
    absolute: bool,
    steps: Vec<StepTemplate>,
}

/// The node types a typed step may name. Programs and projects are named in
/// the path and are never created by a link.
const STEP_TYPES: [NodeType; 4] = [
    NodeType::Case,
    NodeType::Sample,
    NodeType::RawData,
    NodeType::Analysis,
];

impl Template {
    pub fn parse(raw: &str) -> Result<Self, Error> {
        if raw.is_empty() {
            return Err(template_error("--to is empty".into()));
        }
        let (absolute, body) = match raw.strip_prefix('/') {
            Some(rest) => (true, rest),
            None => (false, raw),
        };
        let segments: Vec<&str> = body.split('/').collect();
        if segments.iter().any(|segment| segment.is_empty()) {
            return Err(template_error(format!("{raw}: empty path segment")));
        }
        let mut anchor = Vec::new();
        let mut steps: Vec<StepTemplate> = Vec::new();
        for (position, segment) in segments.iter().enumerate() {
            // The program (or UUID), and the project after a program, are names,
            // not steps. A bare word there is a name, so `/P/case` is a project
            // called `case`. A step with pairs or a mode there is misplaced, not
            // a selector or a project called `case[...]`.
            if position == 0 {
                anchor.push(Text::parse(segment)?);
                continue;
            }
            if absolute && position == 1 {
                let decorated =
                    segment.contains('[') || segment.ends_with('?') || segment.ends_with('+');
                if decorated && parse_step(segment)?.is_some() {
                    return Err(template_error(format!(
                        "{raw}: {segment} is a typed step, and a typed step under a program must follow a project"
                    )));
                }
                anchor.push(Text::parse(segment)?);
                continue;
            }
            match parse_step(segment)? {
                Some(step) => steps.push(step),
                None if !steps.is_empty() => {
                    return Err(template_error(format!(
                        "{raw}: {segment} comes after a typed step; only typed steps can follow one"
                    )));
                }
                None => anchor.push(Text::parse(segment)?),
            }
        }
        let template = Self {
            source: raw.to_string(),
            anchor,
            absolute,
            steps,
        };
        template.check_chain()?;
        Ok(template)
    }

    /// Each typed step is a child of the one before it. A step after `+` must
    /// also create, since nothing can exist under a node that does not yet.
    fn check_chain(&self) -> Result<(), Error> {
        let mut parent = self.anchor_type();
        let mut after_new = false;
        for step in &self.steps {
            if let Some(parent) = parent {
                if step.node_type.parent_type() != Some(parent) {
                    return Err(template_error(self.skip_message(parent, step.node_type)));
                }
            }
            if after_new && step.mode == Mode::Exist {
                return Err(template_error(format!(
                    "{}: {} must exist, but its parent is created by +; write {}? or {}+",
                    self.source,
                    step.node_type.slug(),
                    step.node_type.slug(),
                    step.node_type.slug()
                )));
            }
            after_new |= step.mode == Mode::New;
            parent = Some(step.node_type);
        }
        Ok(())
    }

    fn skip_message(&self, parent: NodeType, child: NodeType) -> String {
        if child.rank() > parent.rank() + 1 {
            let skipped: Vec<NodeType> = NodeType::ALL
                .into_iter()
                .filter(|t| t.rank() > parent.rank() && t.rank() < child.rank())
                .collect();
            let names: Vec<&str> = skipped.iter().map(|t| t.slug()).collect();
            let index = self
                .steps
                .iter()
                .position(|step| step.node_type == child)
                .unwrap_or(0);
            let mode = if self.steps[index..]
                .iter()
                .any(|step| step.mode != Mode::Exist)
            {
                "?"
            } else {
                ""
            };
            let fill: String = skipped
                .iter()
                .map(|t| format!("{}{mode}/", t.slug()))
                .collect();
            let mut suggestion = String::from(if self.absolute { "/" } else { "" });
            let mut parts: Vec<String> = self.anchor.iter().map(Text::to_string).collect();
            for (position, step) in self.steps.iter().enumerate() {
                let mut text = step_text(step);
                if position == index {
                    text = format!("{fill}{text}");
                }
                parts.push(text);
            }
            suggestion.push_str(&parts.join("/"));
            format!(
                "{} → {} skips {}. Did you mean\n  {suggestion}",
                parent.slug(),
                child.slug(),
                names.join(", ")
            )
        } else {
            format!(
                "{}: a {} cannot be under a {}",
                self.source,
                child.slug(),
                parent.slug()
            )
        }
    }

    /// The anchor's type when the template alone says it: `/program` or
    /// `/program/project` with no selectors.
    fn anchor_type(&self) -> Option<NodeType> {
        if !self.absolute {
            return None;
        }
        match self.anchor.as_slice() {
            [_] => Some(NodeType::Program),
            [_, project] if !project.contains_literal(':') => Some(NodeType::Project),
            _ => None,
        }
    }

    pub fn steps(&self) -> Vec<(NodeType, Mode)> {
        self.steps
            .iter()
            .map(|step| (step.node_type, step.mode))
            .collect()
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    /// For `bpm unlink`, which never creates.
    pub fn require_existing(&self) -> Result<(), Error> {
        if self.steps.iter().any(|step| step.mode != Mode::Exist) {
            return Err(template_error(format!(
                "{}: unlink never creates; remove ? and + from the template",
                self.source
            )));
        }
        Ok(())
    }

    fn vars(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.anchor.iter().flat_map(Text::vars).collect();
        for step in &self.steps {
            for (key, value) in &step.pairs {
                names.extend(key.iter().flat_map(Text::vars));
                names.extend(value.iter().flat_map(Text::vars));
            }
        }
        names
    }

    fn render(&self, vars: &Vars) -> Result<(String, Vec<RenderedStep>), String> {
        let mut anchor = Vec::with_capacity(self.anchor.len());
        for segment in &self.anchor {
            anchor.push(segment.render(vars)?);
        }
        let mut address = String::from(if self.absolute { "/" } else { "" });
        address.push_str(&anchor.join("/"));
        let mut steps = Vec::with_capacity(self.steps.len());
        for step in &self.steps {
            let mut selectors = Vec::with_capacity(step.pairs.len());
            for (key, value) in &step.pairs {
                let key = key.as_ref().map(|key| key.render(vars)).transpose()?;
                let value = value.as_ref().map(|value| value.render(vars)).transpose()?;
                for token in key.iter().chain(value.iter()) {
                    validate_meta_token(token).map_err(|_| {
                        format!(
                            "{} [{}:{}] is not a valid metadata pair",
                            step.node_type.slug(),
                            key.as_deref().unwrap_or(""),
                            value.as_deref().unwrap_or("")
                        )
                    })?;
                }
                selectors.push(Selector { key, value });
            }
            steps.push(RenderedStep {
                node_type: step.node_type,
                selectors,
                mode: step.mode,
            });
        }
        Ok((address, steps))
    }
}

impl fmt::Display for Template {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.source)
    }
}

fn step_text(step: &StepTemplate) -> String {
    let mut out = step.node_type.slug().to_string();
    if !step.pairs.is_empty() {
        let pairs: Vec<String> = step
            .pairs
            .iter()
            .map(|(key, value)| {
                format!(
                    "{}:{}",
                    key.as_ref().map(Text::to_string).unwrap_or_default(),
                    value.as_ref().map(Text::to_string).unwrap_or_default()
                )
            })
            .collect();
        out.push_str(&format!("[{}]", pairs.join(", ")));
    }
    out.push_str(step.mode.suffix());
    out
}

/// `TYPE`, `TYPE?`, `TYPE+`, `TYPE[k:v, …]`, `TYPE[k:v]?`, `TYPE[k:v]+`. `None`
/// when the segment is not a typed step.
fn parse_step(segment: &str) -> Result<Option<StepTemplate>, Error> {
    let (head, mode) = match segment.as_bytes().last() {
        Some(b'?') => (&segment[..segment.len() - 1], Mode::Ensure),
        Some(b'+') => (&segment[..segment.len() - 1], Mode::New),
        _ => (segment, Mode::Exist),
    };
    let (word, inner) = match head.find('[') {
        Some(open) => {
            let Some(inner) = head[open + 1..].strip_suffix(']') else {
                return Ok(None);
            };
            (&head[..open], Some(inner))
        }
        None => (head, None),
    };
    let Some(node_type) = NodeType::parse(word) else {
        return Ok(None);
    };
    if !STEP_TYPES.contains(&node_type) {
        return Err(template_error(format!(
            "{segment}: link does not create or step into a {}; name it in the path",
            node_type.slug()
        )));
    }
    let mut pairs = Vec::new();
    if let Some(inner) = inner {
        for raw in inner.split(',') {
            let raw = raw.trim();
            let Some((key, value)) = raw.split_once(':') else {
                return Err(template_error(format!(
                    "{segment}: {raw:?} is not key:value"
                )));
            };
            let key = (!key.is_empty()).then(|| Text::parse(key)).transpose()?;
            let value = (!value.is_empty())
                .then(|| Text::parse(value))
                .transpose()?;
            match (&key, &value, mode) {
                (None, None, _) => {
                    return Err(template_error(format!("{segment}: empty pair")));
                }
                (None, _, Mode::Ensure | Mode::New) | (_, None, Mode::Ensure | Mode::New) => {
                    return Err(template_error(format!(
                        "{segment}: a step that may create needs full key:value pairs, which it writes on the new node"
                    )));
                }
                _ => {}
            }
            pairs.push((key, value));
        }
    }
    Ok(Some(StepTemplate {
        node_type,
        pairs,
        mode,
    }))
}

fn template_error(message: String) -> Error {
    Error::Message(format!("--to: {message}"))
}

/// One typed step with its placeholders filled in.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RenderedStep {
    pub node_type: NodeType,
    pub selectors: Vec<Selector>,
    pub mode: Mode,
}

impl RenderedStep {
    /// The pairs a created node is given. Only steps that may create are
    /// rendered with full pairs.
    pub fn pairs(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = self
            .selectors
            .iter()
            .filter_map(|selector| Some((selector.key.clone()?, selector.value.clone()?)))
            .collect();
        pairs.sort();
        pairs
    }
}

impl fmt::Display for RenderedStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.node_type.slug())?;
        if !self.selectors.is_empty() {
            let pairs: Vec<String> = self
                .selectors
                .iter()
                .map(|selector| {
                    format!(
                        "{}:{}",
                        selector.key.as_deref().unwrap_or(""),
                        selector.value.as_deref().unwrap_or("")
                    )
                })
                .collect();
            write!(f, "[{}]", pairs.join(", "))?;
        }
        f.write_str(self.mode.suffix())
    }
}

/// One catalog location a link operand reaches: the file it belongs to, its
/// URI, and its path relative to the operand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub file_id: String,
    pub uri: String,
    pub relpath: String,
}

/// What one file links to: an address, typed steps below it, and a role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    pub file_id: String,
    pub uri: String,
    pub anchor: String,
    pub steps: Vec<RenderedStep>,
    pub role: String,
}

impl Rendered {
    /// The template with this file's values, through step `upto` (exclusive).
    pub fn display(&self, upto: usize) -> String {
        let mut out = self.anchor.clone();
        for step in &self.steps[..upto] {
            out.push('/');
            out.push_str(&step.to_string());
        }
        out
    }

    pub fn target(&self) -> String {
        self.display(self.steps.len())
    }

    /// The same target and role, whichever location produced it.
    fn same_link(&self, other: &Self) -> bool {
        self.anchor == other.anchor && self.steps == other.steps && self.role == other.role
    }
}

/// What `--match` or `--match-path` is tested against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchOn {
    /// The final path component.
    Name,
    /// The path relative to the operand.
    Path,
}

/// The mapping table: columns become placeholders, and each file picks the
/// one row whose join column equals its rendered join template.
#[derive(Debug)]
pub struct Table {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
    join_column: usize,
    join: Text,
    by_key: HashMap<String, usize>,
}

impl Table {
    /// A `.csv` file is comma-separated; anything else is tab-separated with
    /// no quoting. The first row names the columns. `join` is `COLUMN=TEMPLATE`.
    pub fn load(path: &Path, join: &str) -> Result<Self, Error> {
        let csv = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("csv"));
        let mut builder = csv::ReaderBuilder::new();
        builder.has_headers(true);
        if !csv {
            builder.delimiter(b'\t').quoting(false);
        }
        let table_error =
            |message: String| Error::Message(format!("--table {}: {message}", path.display()));
        let mut reader = builder
            .from_path(path)
            .map_err(|err| table_error(err.to_string()))?;
        let columns: Vec<String> = reader
            .headers()
            .map_err(|err| table_error(err.to_string()))?
            .iter()
            .map(|header| header.trim().to_string())
            .collect();
        let mut seen = HashSet::new();
        for column in &columns {
            if column.is_empty() {
                return Err(table_error("a column has no name".into()));
            }
            if !seen.insert(column.as_str()) {
                return Err(table_error(format!("column {column} appears twice")));
            }
        }
        let mut rows = Vec::new();
        for record in reader.records() {
            let record = record.map_err(|err| table_error(err.to_string()))?;
            rows.push(record.iter().map(str::to_string).collect::<Vec<_>>());
        }
        let Some((column, template)) = join.split_once('=') else {
            return Err(Error::Message(format!(
                "--join {join}: write COLUMN=TEMPLATE, such as barcode={{1}}"
            )));
        };
        let join_column = columns
            .iter()
            .position(|name| name == column)
            .ok_or_else(|| table_error(format!("--join names column {column}, which it lacks")))?;
        let join = Text::parse(template)?;
        let mut by_key = HashMap::new();
        for (index, row) in rows.iter().enumerate() {
            let key = &row[join_column];
            if by_key.insert(key.clone(), index).is_some() {
                return Err(table_error(format!(
                    "{column} {key:?} is on more than one row; a join key must be unique"
                )));
            }
        }
        Ok(Self {
            columns,
            rows,
            join_column,
            join,
            by_key,
        })
    }
}

/// `--expect TYPE=N` or `TYPE=N..M`: every node at that step that the run
/// touches has that many files beneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Expect {
    pub node_type: NodeType,
    pub min: usize,
    pub max: usize,
}

impl Expect {
    pub fn parse(raw: &str) -> Result<Self, Error> {
        let bad = || {
            Error::Message(format!(
                "--expect {raw}: write TYPE=N or TYPE=N..M, such as sample=2"
            ))
        };
        let (node_type, count) = raw.split_once('=').ok_or_else(bad)?;
        let node_type = NodeType::parse(node_type).ok_or_else(bad)?;
        let (min, max) = match count.split_once("..") {
            Some((min, max)) => (
                min.parse().map_err(|_| bad())?,
                max.parse().map_err(|_| bad())?,
            ),
            None => {
                let n = count.parse().map_err(|_| bad())?;
                (n, n)
            }
        };
        if min > max {
            return Err(bad());
        }
        Ok(Self {
            node_type,
            min,
            max,
        })
    }

    pub fn describe(&self) -> String {
        if self.min == self.max {
            self.min.to_string()
        } else {
            format!("{}..{}", self.min, self.max)
        }
    }
}

/// How each source becomes a [`Rendered`] link.
pub struct Renderer {
    template: Template,
    role: Option<Text>,
    pattern: Option<(Regex, MatchOn)>,
    table: Option<Table>,
}

/// The rendered links, one per file, and what kept the others out.
#[derive(Debug, Default)]
pub struct Rendering {
    pub links: Vec<Rendered>,
    /// URIs the pattern did not match.
    pub unmatched: Vec<String>,
    /// One message per file that could not be rendered.
    pub errors: Vec<String>,
    /// Join keys of table rows no file reached.
    pub unused_rows: Vec<String>,
    pub table_rows: usize,
}

const BUILTINS: [&str; 2] = ["basename", "relpath"];

impl Renderer {
    /// Checks every placeholder in the template, the role, and the join
    /// against the names the pattern, the table, and the built-ins define.
    pub fn new(
        template: Template,
        role: Option<&str>,
        pattern: Option<(&str, MatchOn)>,
        table: Option<Table>,
    ) -> Result<Self, Error> {
        let role = role.map(Text::parse).transpose()?;
        let pattern = pattern
            .map(|(raw, on)| {
                Regex::new(&format!("^(?:{raw})$"))
                    .map(|regex| (regex, on))
                    .map_err(|err| Error::Message(format!("--match: {err}")))
            })
            .transpose()?;
        let mut names: HashMap<String, &'static str> = HashMap::new();
        let mut define = |name: String, from: &'static str| -> Result<(), Error> {
            if let Some(other) = names.insert(name.clone(), from) {
                return Err(Error::Message(format!(
                    "placeholder {{{name}}} is defined by both {other} and {from}; rename one"
                )));
            }
            Ok(())
        };
        for builtin in BUILTINS {
            define(builtin.to_string(), "a built-in")?;
        }
        let mut join_names: HashSet<String> = BUILTINS.iter().map(|b| b.to_string()).collect();
        if let Some((regex, _)) = &pattern {
            for number in 0..regex.captures_len() {
                define(number.to_string(), "the pattern")?;
                join_names.insert(number.to_string());
            }
            for name in regex.capture_names().flatten() {
                define(name.to_string(), "the pattern")?;
                join_names.insert(name.to_string());
            }
        }
        if let Some(table) = &table {
            for column in &table.columns {
                if column
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    define(column.clone(), "the table")?;
                }
            }
            for name in table.join.vars() {
                if !join_names.contains(name) {
                    return Err(Error::Message(format!(
                        "--join uses {{{name}}}, which the pattern and the built-ins ({}) do not define",
                        BUILTINS.join(", ")
                    )));
                }
            }
        }
        let used = template
            .vars()
            .into_iter()
            .chain(role.iter().flat_map(Text::vars));
        for name in used {
            if !names.contains_key(name) {
                let hint = if name.chars().all(|c| c.is_ascii_digit()) && pattern.is_none() {
                    "; a numbered placeholder needs --match or --match-path".to_string()
                } else {
                    String::new()
                };
                return Err(Error::Message(format!(
                    "placeholder {{{name}}} is not defined{hint}"
                )));
            }
        }
        Ok(Self {
            template,
            role,
            pattern,
            table,
        })
    }

    pub fn template(&self) -> &Template {
        &self.template
    }

    pub fn render(&self, sources: &[Source]) -> Rendering {
        let mut out = Rendering {
            table_rows: self.table.as_ref().map_or(0, |table| table.rows.len()),
            ..Rendering::default()
        };
        let mut used_rows = HashSet::new();
        let mut by_file: HashMap<String, usize> = HashMap::new();
        for source in sources {
            let rendered = match self.render_one(source, &mut used_rows) {
                Ok(Some(rendered)) => rendered,
                Ok(None) => {
                    out.unmatched.push(source.uri.clone());
                    continue;
                }
                Err(message) => {
                    out.errors.push(format!("{}: {message}", source.uri));
                    continue;
                }
            };
            // One file reached through two locations, or two operands, links
            // once, and only when both give the same target and role.
            match by_file.get(&rendered.file_id) {
                Some(&index) => {
                    let first = &out.links[index];
                    if !first.same_link(&rendered) {
                        out.errors.push(format!(
                            "{} and {} are one file but give different links: {} ({}) and {} ({})",
                            first.uri,
                            rendered.uri,
                            first.target(),
                            first.role,
                            rendered.target(),
                            rendered.role
                        ));
                    }
                }
                None => {
                    by_file.insert(rendered.file_id.clone(), out.links.len());
                    out.links.push(rendered);
                }
            }
        }
        if let Some(table) = &self.table {
            for (index, row) in table.rows.iter().enumerate() {
                if !used_rows.contains(&index) {
                    out.unused_rows.push(row[table.join_column].clone());
                }
            }
        }
        out
    }

    fn render_one(
        &self,
        source: &Source,
        used_rows: &mut HashSet<usize>,
    ) -> Result<Option<Rendered>, String> {
        let basename = source
            .relpath
            .rsplit('/')
            .next()
            .unwrap_or(&source.relpath)
            .to_string();
        let mut vars = Vars::default();
        vars.set("basename", Some(basename.clone()));
        vars.set("relpath", Some(source.relpath.clone()));
        if let Some((regex, on)) = &self.pattern {
            let subject = match on {
                MatchOn::Name => &basename,
                MatchOn::Path => &source.relpath,
            };
            let Some(captures) = regex.captures(subject) else {
                return Ok(None);
            };
            for (number, group) in captures.iter().enumerate() {
                vars.set(
                    &number.to_string(),
                    group.map(|group| group.as_str().to_string()),
                );
            }
            for name in regex.capture_names().flatten() {
                vars.set(
                    name,
                    captures.name(name).map(|group| group.as_str().to_string()),
                );
            }
        }
        if let Some(table) = &self.table {
            let key = table.join.render(&vars)?;
            let Some(&row) = table.by_key.get(&key) else {
                return Err(format!(
                    "no table row has {} {key:?}",
                    table.columns[table.join_column]
                ));
            };
            used_rows.insert(row);
            for (column, value) in table.columns.iter().zip(&table.rows[row]) {
                vars.set(column, Some(value.clone()));
            }
        }
        let (anchor, steps) = self.template.render(&vars)?;
        let role = match &self.role {
            Some(role) => role.render(&vars)?,
            None => String::new(),
        };
        Ok(Some(Rendered {
            file_id: source.file_id.clone(),
            uri: source.uri.clone(),
            anchor,
            steps,
            role,
        }))
    }
}

/// Placeholder values for one file. `None` is a capture group that did not
/// take part in the match.
#[derive(Default)]
struct Vars(HashMap<String, Option<String>>);

impl Vars {
    fn set(&mut self, name: &str, value: Option<String>) {
        self.0.insert(name.to_string(), value);
    }

    fn get(&self, name: &str) -> Result<&str, String> {
        match self.0.get(name) {
            Some(Some(value)) => Ok(value),
            Some(None) => Err(format!("capture {{{name}}} did not take part in the match")),
            None => Err(format!("placeholder {{{name}}} has no value")),
        }
    }
}

/// Roles are lowercase letters, digits, `_`, and `-`.
pub fn validate_role(role: &str) -> Result<(), String> {
    if role.is_empty() {
        return Err("a link role cannot be empty".into());
    }
    if !role
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(format!(
            "role {role:?} must be lowercase letters, digits, _ and -"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(relpath: &str) -> Source {
        Source {
            file_id: relpath.to_string(),
            uri: format!("/data/{relpath}"),
            relpath: relpath.to_string(),
        }
    }

    #[test]
    fn typed_steps_and_modes() {
        let template =
            Template::parse("/P/J/case[subject_id:{1}]?/sample?/raw_data[read:{2}, lane:L1]+")
                .unwrap();
        assert_eq!(
            template.steps(),
            vec![
                (NodeType::Case, Mode::Ensure),
                (NodeType::Sample, Mode::Ensure),
                (NodeType::RawData, Mode::New),
            ]
        );
        assert_eq!(template.anchor.len(), 2);
    }

    #[test]
    fn a_bare_word_after_the_program_is_a_project_name() {
        let template = Template::parse("/P/case").unwrap();
        assert!(template.steps.is_empty());
        assert_eq!(template.anchor.len(), 2);
        assert!(Template::parse("/P/case[subject_id:X]").is_err());
    }

    #[test]
    fn skipped_levels_are_refused_with_the_full_path() {
        let err = Template::parse("/P/J/case[subject_id:{1}]/analysis+")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("case → analysis skips sample, raw_data"),
            "{err}"
        );
        assert!(
            err.contains("/P/J/case[subject_id:{1}]/sample?/raw_data?/analysis+"),
            "{err}"
        );
        let err = Template::parse("/P/J/sample").unwrap_err().to_string();
        assert!(err.contains("project → sample skips case"), "{err}");
        let err = Template::parse("/P/J/case/case").unwrap_err().to_string();
        assert!(err.contains("cannot be under"), "{err}");
    }

    #[test]
    fn typed_steps_close_the_template() {
        assert!(Template::parse("/P/J/case/subject_id:X").is_err());
        assert!(Template::parse("/P/J/project").is_err());
        assert!(Template::parse("/P/J/case+/sample").is_err());
        assert!(Template::parse("/P/J/case[subject_id:]?").is_err());
        assert!(Template::parse("/P/J/case[subject_id:]").is_ok());
        // An untyped selector anchor leaves the type check to resolution.
        assert!(Template::parse("/P/J/subject_id:X/sample?").is_ok());
        let id = uuid::Uuid::now_v7();
        assert!(Template::parse(&format!("{id}/raw_data+")).is_ok());
    }

    #[test]
    fn placeholders_must_be_defined() {
        let template = Template::parse("/P/J/case[subject_id:{1}]").unwrap();
        assert!(Renderer::new(template.clone(), Some("data"), None, None).is_err());
        assert!(
            Renderer::new(
                template,
                Some("data"),
                Some(("(\\w+)\\.bam", MatchOn::Name)),
                None
            )
            .is_ok()
        );
        let template = Template::parse("/P/J/case[subject_id:{subj}]").unwrap();
        assert!(
            Renderer::new(
                template,
                Some("{nope}"),
                Some(("(?<subj>\\w+)", MatchOn::Name)),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn rendering_captures_the_whole_name() {
        let template =
            Template::parse("/P/J/case[subject_id:{1}]/sample?/raw_data[read:{2}]+").unwrap();
        let renderer = Renderer::new(
            template,
            Some("data"),
            Some(("([A-Za-z0-9]+)_x_(R[12])\\.fq\\.gz", MatchOn::Name)),
            None,
        )
        .unwrap();
        let rendering = renderer.render(&[
            source("S1_x_R1.fq.gz"),
            source("sub/S1_x_R2.fq.gz"),
            source("S1_x_R1.fq.gz.md5"),
        ]);
        assert_eq!(rendering.unmatched, vec!["/data/S1_x_R1.fq.gz.md5"]);
        assert_eq!(rendering.links.len(), 2);
        assert_eq!(
            rendering.links[1].target(),
            "/P/J/case[subject_id:S1]/sample?/raw_data[read:R2]+"
        );
    }

    #[test]
    fn match_path_sees_directories() {
        let template = Template::parse("/P/J/case[subject_id:{subj}]").unwrap();
        let renderer = Renderer::new(
            template,
            Some("data"),
            Some(("(?<subj>[^/]+)/.*\\.bam", MatchOn::Path)),
            None,
        )
        .unwrap();
        let rendering = renderer.render(&[source("S7/aln/x.bam")]);
        assert_eq!(rendering.links[0].target(), "/P/J/case[subject_id:S7]");
    }

    #[test]
    fn a_value_with_a_slash_or_colon_is_refused() {
        let template = Template::parse("/P/J/case[subject_id:{1}]").unwrap();
        let renderer =
            Renderer::new(template, Some("data"), Some(("(.*)", MatchOn::Path)), None).unwrap();
        let rendering = renderer.render(&[source("a/b")]);
        assert_eq!(rendering.errors.len(), 1, "{:?}", rendering.errors);
    }

    #[test]
    fn expect_spelling() {
        let expect = Expect::parse("sample=2").unwrap();
        assert_eq!((expect.min, expect.max), (2, 2));
        let expect = Expect::parse("raw_data=1..3").unwrap();
        assert_eq!((expect.min, expect.max), (1, 3));
        assert!(Expect::parse("sample").is_err());
        assert!(Expect::parse("thing=2").is_err());
        assert!(Expect::parse("sample=3..1").is_err());
    }

    #[test]
    fn roles_are_normalized_spelling() {
        assert!(validate_role("data").is_ok());
        assert!(validate_role("qc-report_2").is_ok());
        assert!(validate_role("Data").is_err());
        assert!(validate_role("").is_err());
        assert!(validate_role("raw data").is_err());
    }
}
