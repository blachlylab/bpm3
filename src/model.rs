//! Entity rules that do not need a database: names, metadata tokens, and addresses.

use std::collections::BTreeMap;

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
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// A command address: a `/program[/project][/selector...]` path, or a UUID with
/// optional selector segments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    Path {
        program: String,
        project: Option<String>,
        selectors: Vec<Selector>,
    },
    Id {
        id: Uuid,
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
        let id = Uuid::parse_str(parts.next().unwrap_or("")).map_err(|_| ModelError::Address)?;
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityRow {
    pub node_type: NodeType,
    pub id: Uuid,
    pub path: Option<String>,
    pub name: Option<String>,
    pub metadata: BTreeMap<String, String>,
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
    fn uuid_address_is_not_a_program_name() {
        let id = Uuid::now_v7();
        assert_eq!(id.get_version(), Some(uuid::Version::SortRand));
        let addr = parse_address(&format!("{id}/sample_kind:aliquot")).unwrap();
        match addr {
            Address::Id {
                id: parsed,
                selectors,
            } => {
                assert_eq!(parsed, id);
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
