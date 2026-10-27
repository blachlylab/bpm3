//! Entity rules that do not need a database: names, metadata tokens,
//! addresses, and short ids.

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use uuid::Uuid;

/// The six tree types, in parent-to-child order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeType {
    Program,
    Project,
    Case,
    Sample,
    RawData,
    Analysis,
}

impl NodeType {
    pub const ALL: [Self; 6] = [
        Self::Program,
        Self::Project,
        Self::Case,
        Self::Sample,
        Self::RawData,
        Self::Analysis,
    ];

    /// The label the UI shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Program => "Program",
            Self::Project => "Project",
            Self::Case => "Case",
            Self::Sample => "Sample",
            Self::RawData => "Raw Data",
            Self::Analysis => "Analysis",
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::Program => "program",
            Self::Project => "project",
            Self::Case => "case",
            Self::Sample => "sample",
            Self::RawData => "raw_data",
            Self::Analysis => "analysis",
        }
    }

    pub fn parse(slug: &str) -> Option<Self> {
        Some(match slug {
            "program" => Self::Program,
            "project" => Self::Project,
            "case" => Self::Case,
            "sample" => Self::Sample,
            "raw_data" => Self::RawData,
            "analysis" => Self::Analysis,
            _ => return None,
        })
    }

    /// Parent type required by the tree. A program has none.
    pub fn parent_type(self) -> Option<Self> {
        match self {
            Self::Program => None,
            Self::Project => Some(Self::Program),
            Self::Case => Some(Self::Project),
            Self::Sample => Some(Self::Case),
            Self::RawData => Some(Self::Sample),
            Self::Analysis => Some(Self::RawData),
        }
    }

    pub fn takes_name(self) -> bool {
        matches!(self, Self::Program | Self::Project)
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::Program => 0,
            Self::Project => 1,
            Self::Case => 2,
            Self::Sample => 3,
            Self::RawData => 4,
            Self::Analysis => 5,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ModelError {
    #[error("name must be 1–256 characters")]
    NameLength,
    #[error("name cannot contain '/' or ':' or control characters")]
    NameChars,
    #[error("name cannot have leading or trailing whitespace")]
    NameWhitespace,
    #[error("metadata key and value cannot be empty or contain ':' or '/'")]
    Metadata,
    #[error("invalid selector")]
    Selector,
    #[error("invalid address")]
    Address,
    #[error("a digest is algorithm:hex, such as blake3:<hex>")]
    Digest,
}

/// A program or project name: 1–256 characters, no `/` or `:`, no ASCII controls,
/// no leading or trailing whitespace. The value is kept as written.
pub fn validate_name(name: &str) -> Result<(), ModelError> {
    let len = name.chars().count();
    if !(1..=256).contains(&len) {
        return Err(ModelError::NameLength);
    }
    if name
        .chars()
        .any(|c| c == '/' || c == ':' || c.is_ascii_control())
    {
        return Err(ModelError::NameChars);
    }
    let mut chars = name.chars();
    if chars.next().is_some_and(char::is_whitespace)
        || chars.next_back().is_some_and(char::is_whitespace)
    {
        return Err(ModelError::NameWhitespace);
    }
    Ok(())
}

/// Metadata keys and values are non-empty and cannot contain `:` or `/`.
pub fn validate_meta_token(token: &str) -> Result<(), ModelError> {
    if token.is_empty() || token.contains(':') || token.contains('/') {
        return Err(ModelError::Metadata);
    }
    Ok(())
}

/// `key:value`, `key:` (key present), or `:value` (value on any key).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Selector {
    pub key: Option<String>,
    pub value: Option<String>,
}

impl Selector {
    pub fn matches(&self, metadata: &BTreeMap<String, String>) -> bool {
        match (&self.key, &self.value) {
            (Some(key), Some(value)) => metadata.get(key).is_some_and(|v| v == value),
            (Some(key), None) => metadata.contains_key(key),
            (None, Some(value)) => metadata.values().any(|v| v == value),
            (None, None) => false,
        }
    }
}

pub fn parse_selector(raw: &str) -> Result<Selector, ModelError> {
    let Some((key, value)) = raw.split_once(':') else {
        return Err(ModelError::Selector);
    };
    // A second colon would be inside `value` after split_once, and validation rejects it.
    match (key.is_empty(), value.is_empty()) {
        (false, false) => {
            validate_meta_token(key)?;
            validate_meta_token(value)?;
            Ok(Selector {
                key: Some(key.to_string()),
                value: Some(value.to_string()),
            })
        }
        (false, true) => {
            validate_meta_token(key)?;
            Ok(Selector {
                key: Some(key.to_string()),
                value: None,
            })
        }
        (true, false) => {
            validate_meta_token(value)?;
            Ok(Selector {
                key: None,
                value: Some(value.to_string()),
            })
        }
        _ => Err(ModelError::Selector),
    }
}

/// Hex digits in a short id: the last group of a hyphenated UUID. In a v7 id
/// that group is the low 16 bits of the per-millisecond counter and 32 random
/// bits, so ids made in the same millisecond never share it.
pub const SHORT_ID_LEN: usize = 12;

/// The last [`SHORT_ID_LEN`] hex digits of an id, lowercase. Made only by
/// [`short_id`] and [`parse_id`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ShortId(String);

impl ShortId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ShortId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// An id as a command names it: in full, or by its short id. The catalog
/// resolves either to one row, or refuses a short id that several rows share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdInput {
    Full(Uuid),
    Short(ShortId),
}

impl From<Uuid> for IdInput {
    fn from(id: Uuid) -> Self {
        Self::Full(id)
    }
}

impl fmt::Display for IdInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(id) => write!(f, "{id}"),
            Self::Short(short) => write!(f, "{short}"),
        }
    }
}

/// The short id of `id`.
pub fn short_id(id: Uuid) -> ShortId {
    let text = id.hyphenated().to_string();
    ShortId(text[text.len() - SHORT_ID_LEN..].to_string())
}

/// An id typed on the command line: a UUID, or exactly [`SHORT_ID_LEN`] hex
/// digits in either case. Anything else is not an id.
pub fn parse_id(raw: &str) -> Option<IdInput> {
    if raw.len() == SHORT_ID_LEN && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(IdInput::Short(ShortId(raw.to_ascii_lowercase())));
    }
    Uuid::parse_str(raw).ok().map(IdInput::Full)
}

/// How a table shows an id: its short id, or the full UUID when `shared`
/// says another row has the same short id, so what is shown always resolves.
pub fn shown_id(id: Uuid, shared: &HashSet<Uuid>) -> String {
    if shared.contains(&id) {
        id.to_string()
    } else {
        short_id(id).to_string()
    }
}

/// A command address: a `/program[/project][/selector...]` path, or an id
/// (full or short) with optional selector segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    Path {
        program: String,
        project: Option<String>,
        selectors: Vec<Selector>,
    },
    Id {
        id: IdInput,
        selectors: Vec<Selector>,
    },
}

pub fn parse_address(input: &str) -> Result<Address, ModelError> {
    if input.is_empty() {
        return Err(ModelError::Address);
    }
    if let Some(rest) = input.strip_prefix('/') {
        if rest.is_empty() {
            return Err(ModelError::Address);
        }
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.iter().any(|p| p.is_empty()) {
            return Err(ModelError::Address);
        }
        validate_name(parts[0])?;
        let mut index = 1;
        let mut project = None;
        if parts.len() > 1 && !parts[1].contains(':') {
            validate_name(parts[1])?;
            project = Some(parts[1].to_string());
            index = 2;
        }
        let mut selectors = Vec::new();
        for part in &parts[index..] {
            selectors.push(parse_selector(part)?);
        }
        Ok(Address::Path {
            program: parts[0].to_string(),
            project,
            selectors,
        })
    } else {
        let mut parts = input.split('/');
        let id = parse_id(parts.next().unwrap_or("")).ok_or(ModelError::Address)?;
        let mut selectors = Vec::new();
        for part in parts {
            if part.is_empty() {
                return Err(ModelError::Address);
            }
            selectors.push(parse_selector(part)?);
        }
        Ok(Address::Id { id, selectors })
    }
}

/// One entity as query returns it. `path` is set for programs and projects.
/// `project` is the path of the Project, or the Program, a lower entity sits
/// in; it is unset on programs and projects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityRow {
    pub node_type: NodeType,
    pub id: Uuid,
    pub path: Option<String>,
    pub name: Option<String>,
    pub parent: Option<ParentRef>,
    pub project: Option<String>,
    pub metadata: BTreeMap<String, String>,
}

/// An entity's parent: its type and id, and its path when it is a Program or
/// a Project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentRef {
    pub node_type: NodeType,
    pub id: Uuid,
    pub path: Option<String>,
}

/// The local filesystem backend. The only one this milestone writes.
pub const POSIX: &str = "posix";

/// Backend names a `bpm scan` argument can be. `s3` selects nothing until the
/// object-store milestone writes such locations.
pub const BACKENDS: [&str; 2] = [POSIX, "s3"];

/// A quick fingerprint and the scheme that produced it. Two fingerprints are
/// compared only when the schemes are equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub scheme: String,
    pub hex: String,
}

/// How a command names one file: its id, or one of its locations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRef {
    Id(IdInput),
    Location { backend: String, uri: String },
}

/// The operator drift states of one location (PRD §4.6), plus `unverified`
/// for a present, unchanged location whose bytes have not been hashed yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Drift {
    Missing,
    StatChanged,
    DigestMismatch,
    Ok,
    Unverified,
}

impl Drift {
    pub const ALL: [Self; 5] = [
        Self::Ok,
        Self::Unverified,
        Self::Missing,
        Self::StatChanged,
        Self::DigestMismatch,
    ];

    pub fn parse(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.slug() == slug)
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::StatChanged => "stat_changed",
            Self::DigestMismatch => "digest_mismatch",
            Self::Ok => "ok",
            Self::Unverified => "unverified",
        }
    }

    /// The states one location is in, from its three columns. A location can
    /// be both `stat_changed` and `digest_mismatch`.
    pub fn of(presence: &str, stat_state: &str, digest_state: &str) -> Vec<Self> {
        let present = presence == "present";
        let mut states = Vec::new();
        if !present {
            states.push(Self::Missing);
        }
        if present && stat_state == "changed" {
            states.push(Self::StatChanged);
        }
        if digest_state == "mismatch" {
            states.push(Self::DigestMismatch);
        }
        if present && stat_state == "unchanged" && digest_state == "match" {
            states.push(Self::Ok);
        }
        if states.is_empty() {
            states.push(Self::Unverified);
        }
        states
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationRow {
    pub backend: String,
    pub uri: String,
    pub presence: String,
    pub stat_state: String,
    pub digest_state: String,
    pub last_seen_at: String,
}

impl LocationRow {
    pub fn drift(&self) -> Vec<Drift> {
        Drift::of(&self.presence, &self.stat_state, &self.digest_state)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRow {
    pub node_type: NodeType,
    pub node_id: Uuid,
    pub role: String,
}

/// One current or historical digest. The wire form is `algorithm:hex`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestRow {
    pub algorithm: String,
    pub hex: String,
    pub source: String,
    pub generation: i64,
    pub current: bool,
}

impl DigestRow {
    pub fn wire(&self) -> String {
        format!("{}:{}", self.algorithm, self.hex)
    }
}

/// One file as query returns it. `digests` holds the current digests only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    pub id: Uuid,
    pub size: Option<i64>,
    pub mtime: Option<String>,
    pub fingerprint: Option<Fingerprint>,
    pub digests: Vec<DigestRow>,
    pub locations: Vec<LocationRow>,
    pub links: Vec<LinkRow>,
}

impl FileRow {
    /// Every state any location is in, each once, in a fixed order.
    pub fn drift(&self) -> Vec<Drift> {
        let mut states: Vec<Drift> = self.locations.iter().flat_map(LocationRow::drift).collect();
        states.sort();
        states.dedup();
        states
    }
}

/// `algorithm:hex`, as `bpm query files --digest` and the search page take it.
/// The algorithm and the hex are compared in lowercase.
pub fn parse_digest(raw: &str) -> Result<(String, String), ModelError> {
    let (algorithm, hex) = raw.split_once(':').ok_or(ModelError::Digest)?;
    if algorithm.is_empty() || hex.is_empty() || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ModelError::Digest);
    }
    Ok((algorithm.to_ascii_lowercase(), hex.to_ascii_lowercase()))
}

/// Which slice of a result a caller wants. Pages are the read-only UI's; the
/// CLI asks for everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub offset: usize,
    pub limit: usize,
}

impl Window {
    pub const ALL: Self = Self {
        offset: 0,
        limit: usize::MAX,
    };

    pub fn page(number: usize, size: usize) -> Self {
        Self {
            offset: number.saturating_sub(1).saturating_mul(size),
            limit: size,
        }
    }

    pub fn apply<T>(self, rows: Vec<T>) -> Vec<T> {
        rows.into_iter()
            .skip(self.offset)
            .take(self.limit)
            .collect()
    }
}

/// One window of a result, and how many rows the whole result has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paged<T> {
    pub total: usize,
    pub rows: Vec<T>,
}

/// `bpm query summary` and the UI's home page (PRD §4.8).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    /// Every node type, in tree order, with its count.
    pub entities: Vec<(NodeType, i64)>,
    pub files: i64,
    pub bytes: i64,
    pub unlinked: i64,
    /// Files linked directly to entities of each type, and their bytes. A
    /// file linked to two entities of one type counts once for that type.
    pub linked_by_type: Vec<(NodeType, i64, i64)>,
    pub locations_by_backend: Vec<(String, i64)>,
    /// Locations in each drift state. One location can be in two states.
    pub drift: Vec<(Drift, i64)>,
    pub sample_kind: Vec<(String, i64)>,
    pub assay: Vec<(String, i64)>,
    /// Links by role, so a second spelling of a role is easy to spot.
    pub roles: Vec<(String, i64)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_prd() {
        assert!(validate_name("CLL").is_ok());
        assert!(validate_name("WES-relapse").is_ok());
        assert!(validate_name(&"a".repeat(256)).is_ok());
        assert_eq!(validate_name(""), Err(ModelError::NameLength));
        assert_eq!(validate_name(&"a".repeat(257)), Err(ModelError::NameLength));
        assert_eq!(validate_name("a/b"), Err(ModelError::NameChars));
        assert_eq!(validate_name("a:b"), Err(ModelError::NameChars));
        assert_eq!(validate_name("a\nb"), Err(ModelError::NameChars));
        assert_eq!(validate_name(" CLL"), Err(ModelError::NameWhitespace));
        assert_eq!(validate_name("CLL "), Err(ModelError::NameWhitespace));
        // Do not trim: the surrounding space is the whole rejection.
        assert!(" CLL".trim() == "CLL");
    }

    #[test]
    fn selectors_and_paths() {
        assert_eq!(
            parse_selector("subject_id:CLL-001").unwrap(),
            Selector {
                key: Some("subject_id".into()),
                value: Some("CLL-001".into())
            }
        );
        assert_eq!(
            parse_selector("subject_id:").unwrap(),
            Selector {
                key: Some("subject_id".into()),
                value: None
            }
        );
        assert_eq!(
            parse_selector(":CLL-001").unwrap(),
            Selector {
                key: None,
                value: Some("CLL-001".into())
            }
        );
        assert!(parse_selector("a:b:c").is_err());
        assert!(parse_selector(":").is_err());
        assert!(parse_selector("no-colon").is_err());

        let path = parse_address("/CLL/WES-relapse/ext_id:CLL-001/sample_kind:aliquot").unwrap();
        match path {
            Address::Path {
                program,
                project,
                selectors,
            } => {
                assert_eq!(program, "CLL");
                assert_eq!(project.as_deref(), Some("WES-relapse"));
                assert_eq!(selectors.len(), 2);
            }
            Address::Id { .. } => panic!("path parsed as id"),
        }

        // A selector directly under the program is not a project name.
        let under_program = parse_address("/CLL/subject_id:CLL-001").unwrap();
        match under_program {
            Address::Path {
                project, selectors, ..
            } => {
                assert!(project.is_none());
                assert_eq!(selectors.len(), 1);
            }
            Address::Id { .. } => panic!("path parsed as id"),
        }
    }

    #[test]
    fn a_short_id_is_the_last_group_and_parses_back() {
        let id = Uuid::parse_str("01a116f9-9f5e-70ec-a2c9-b393d8bb4c4e").unwrap();
        let short = short_id(id);
        assert_eq!(short.as_str(), "b393d8bb4c4e");
        assert_eq!(
            parse_id("b393d8bb4c4e"),
            Some(IdInput::Short(short.clone()))
        );
        assert_eq!(parse_id("B393D8BB4C4E"), Some(IdInput::Short(short)));
        assert_eq!(parse_id(&id.to_string()), Some(IdInput::Full(id)));
        // Exactly twelve hex digits, nothing shorter, longer, or not hex.
        for raw in [
            "b393d8bb4c4",
            "b393d8bb4c4e0",
            "b393d8bb4c4g",
            "",
            "notes.txt",
        ] {
            assert_eq!(parse_id(raw), None, "{raw}");
        }
        let mut shared = HashSet::new();
        assert_eq!(shown_id(id, &shared), "b393d8bb4c4e");
        shared.insert(id);
        assert_eq!(shown_id(id, &shared), id.to_string());
        let addr = parse_address("b393d8bb4c4e/sample_kind:aliquot").unwrap();
        assert!(matches!(
            addr,
            Address::Id {
                id: IdInput::Short(_),
                ..
            }
        ));
    }

    #[test]
    fn uuid_address_is_not_a_program_name() {
        let id = Uuid::now_v7();
        assert_eq!(id.get_version(), Some(uuid::Version::SortRand));
        let addr = parse_address(&format!("{id}/sample_kind:aliquot")).unwrap();
        match addr {
            Address::Id {
                id: parsed,
                selectors,
            } => {
                assert_eq!(parsed, IdInput::Full(id));
                assert_eq!(selectors.len(), 1);
            }
            Address::Path { .. } => panic!("uuid parsed as a path"),
        }
        // The same text with a leading slash is a program name, and it fails
        // because a name cannot be parsed as a selector-less path of a uuid
        // only when it contains characters names reject. A uuid is a legal name,
        // so it is a program path, not an id lookup.
        let as_path = parse_address(&format!("/{id}")).unwrap();
        assert!(matches!(as_path, Address::Path { .. }));
    }
}
