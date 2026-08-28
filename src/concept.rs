//! OKF concepts: markdown documents with optional YAML frontmatter (spec §1.4).

use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use snafu::{IntoError, OptionExt, ResultExt, ensure};
use yaml_serde::{Mapping, Value};

use crate::error::{Error, IoSnafu, MissingFrontmatterSnafu, Result, ValidationSnafu, YamlSnafu};

/// Maximum frontmatter nesting depth `Concept::new` accepts. Exceeding the YAML
/// emitter's recursion limit would make `to_string` panic, so construction enforces it.
const MAX_FRONTMATTER_DEPTH: usize = 100;

fn yaml_depth(value: &Value) -> usize {
    /// Recursion is bounded so adversarially deep YAML cannot overflow the
    /// stack here (the YAML parser has its own limit, but this stays safe
    /// regardless); anything at or past the cap reads as the cap.
    fn go(value: &Value, remaining: usize) -> usize {
        if remaining == 0 {
            return MAX_FRONTMATTER_DEPTH + 1;
        }
        match value {
            Value::Mapping(m) => 1 + m.values().map(|v| go(v, remaining - 1)).max().unwrap_or(0),
            Value::Sequence(s) => 1 + s.iter().map(|v| go(v, remaining - 1)).max().unwrap_or(0),
            _ => 1,
        }
    }
    go(value, MAX_FRONTMATTER_DEPTH + 1)
}

/// One markdown-plus-frontmatter document.
///
/// The frontmatter is kept as an ordered [`Mapping`] so unknown keys survive a
/// parse → serialize round-trip untouched (`STR-6`); no argosy-typed schema is
/// imposed at this layer.
#[derive(Debug, Clone, PartialEq)]
pub struct Concept {
    frontmatter: Mapping,
    body: String,
}

impl Concept {
    /// Builds a concept directly from its parts.
    ///
    /// Rejects frontmatter nested too deeply for the YAML emitter, so the
    /// infallible `to_string`/`to_file` contract below is genuinely upheld.
    pub fn new(frontmatter: Mapping, body: String) -> Result<Self> {
        let depth = yaml_depth(&Value::Mapping(frontmatter.clone()));
        ensure!(
            depth <= MAX_FRONTMATTER_DEPTH,
            ValidationSnafu {
                reason: format!(
                    "frontmatter nesting depth {depth} exceeds limit {MAX_FRONTMATTER_DEPTH}"
                )
            }
        );
        Ok(Self { frontmatter, body })
    }

    /// The raw frontmatter mapping, preserving unknown keys in order.
    pub fn frontmatter(&self) -> &Mapping {
        &self.frontmatter
    }

    /// The markdown body, verbatim.
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Parses a concept from a string. A leading `---` line opens the
    /// frontmatter block; anything else makes the whole input the body.
    // Inherent companion to the `FromStr` impl — allows calling without importing the trait.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(input: &str) -> Result<Self> {
        Self::parse(input, None)
    }

    /// Reads and parses a concept from a file, attaching the path to errors.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).context(IoSnafu {
            path: path.to_path_buf(),
        })?;
        Self::parse(&text, Some(path.to_path_buf()))
    }

    fn parse(input: &str, path: Option<PathBuf>) -> Result<Self> {
        // Tolerate a UTF-8 BOM (Windows editors) rather than losing the
        // frontmatter entirely.
        let input = input.strip_prefix('\u{feff}').unwrap_or(input);
        let first_line = input.lines().next().unwrap_or("");
        if first_line.trim_end_matches(['\r', '\n']) != "---" {
            return Ok(Self {
                frontmatter: Mapping::new(),
                body: input.to_string(),
            });
        }

        // Start of the YAML block: just past the first line's newline.
        let fm_start = input.find('\n').map(|i| i + 1).unwrap_or(input.len());
        let rest = &input[fm_start..];

        let mut offset = 0;
        for line in rest.split_inclusive('\n') {
            if line.trim_end_matches(['\r', '\n']) == "---" {
                let yaml_text = &rest[..offset];
                let body = rest[offset + line.len()..].to_string();
                let frontmatter = if yaml_text.trim().is_empty() {
                    Mapping::new()
                } else {
                    let value: Value =
                        yaml_serde::from_str(yaml_text).map_err(|source| match &path {
                            Some(p) => YamlSnafu { path: p.clone() }.into_error(source),
                            None => ValidationSnafu {
                                reason: format!("failed to parse YAML frontmatter: {source}"),
                            }
                            .build(),
                        })?;
                    match value {
                        Value::Mapping(m) => m,
                        // A scalar or sequence block is malformed frontmatter.
                        _ => return MissingFrontmatterSnafu { path }.fail(),
                    }
                };
                // Route through `new` so parsed concepts get the same
                // frontmatter depth guard as constructed ones — `to_string`'s
                // infallible contract depends on it.
                return Self::new(frontmatter, body);
            }
            offset += line.len();
        }

        // Opener without a closing delimiter.
        MissingFrontmatterSnafu { path }.fail()
    }

    /// Serializes back to the markdown-plus-frontmatter form. Unknown keys and
    /// the body round-trip untouched; YAML block formatting may normalize.
    // Inherent `to_string` is the spec'd serialization API; no `Display` impl is wanted.
    #[allow(clippy::inherent_to_string)]
    pub fn to_string(&self) -> String {
        if self.frontmatter.is_empty() {
            return self.body.clone();
        }
        // Cannot fail: every construction path (`new`, `parse`) depth-checks
        // the frontmatter against the emitter's recursion limit.
        let yaml = yaml_serde::to_string(&self.frontmatter)
            .expect("frontmatter depth is validated at construction");
        format!("---\n{yaml}---\n{}", self.body)
    }

    /// Writes the serialized concept to a file.
    pub fn to_file(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        std::fs::write(path, self.to_string()).context(IoSnafu {
            path: path.to_path_buf(),
        })
    }

    /// Looks up any frontmatter key, including unknown/custom ones.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.frontmatter.get(key)
    }

    /// Looks up a frontmatter key expected to hold a string.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key)?.as_str()
    }

    /// The frontmatter `type` field.
    pub fn concept_type(&self) -> Option<&str> {
        self.get_str("type")
    }

    /// The frontmatter `description` field.
    pub fn description(&self) -> Option<&str> {
        self.get_str("description")
    }

    /// The frontmatter `tags` field, accepting a YAML sequence or a single
    /// string. Absent or ill-typed values yield an empty vec; non-string
    /// items inside a sequence are skipped.
    pub fn tags(&self) -> Vec<&str> {
        match self.get("tags") {
            Some(Value::Sequence(items)) => items.iter().filter_map(Value::as_str).collect(),
            Some(Value::String(s)) => vec![s.as_str()],
            _ => Vec::new(),
        }
    }

    /// True iff frontmatter exists with a non-empty `type` — the only hard OKF
    /// concept requirement argosy depends on (spec §1.5, `STR-5`).
    pub fn is_okf_conformant(&self) -> bool {
        !self.frontmatter.is_empty() && self.concept_type().is_some_and(|t| !t.trim().is_empty())
    }
}

impl FromStr for Concept {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self> {
        Self::parse(s, None)
    }
}

/// A concept's identity within one bundle: its path relative to the bundle
/// root, `.md` stripped, forward-slash separated (e.g.
/// `document/decisions/2026-05-caching`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConceptId(String);

impl serde::Serialize for ConceptId {
    /// Serializes as the slash-separated id string.
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl ConceptId {
    /// The slash-separated id.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Renders the id back to a relative path, restoring the `.md` extension.
    pub fn to_relative_path(&self) -> PathBuf {
        // Forward slashes are valid separators on all supported platforms.
        PathBuf::from(format!("{}.md", self.0))
    }
}

impl FromStr for ConceptId {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self> {
        let stripped = raw.strip_suffix(".md").unwrap_or(raw);
        let mut id = String::new();
        for segment in stripped.split('/') {
            match segment {
                "" => {
                    return ValidationSnafu {
                        reason: format!(
                            "invalid concept id `{raw}`: empty segment (absolute path or doubled separator?)"
                        ),
                    }
                    .fail();
                }
                "." => continue,
                ".." => {
                    return ValidationSnafu {
                        reason: format!("invalid concept id `{raw}`: `..` is not allowed"),
                    }
                    .fail();
                }
                s if s.contains(['\\', ':']) => {
                    // `:` would render as a drive-qualified absolute path on Windows.
                    return ValidationSnafu {
                        reason: format!("invalid concept id `{raw}`: `\\` and `:` are not allowed"),
                    }
                    .fail();
                }
                s => {
                    if !id.is_empty() {
                        id.push('/');
                    }
                    id.push_str(s);
                }
            }
        }
        if id.is_empty() {
            return ValidationSnafu {
                reason: format!("invalid concept id `{raw}`: no segments"),
            }
            .fail();
        }
        Ok(Self(id))
    }
}

impl TryFrom<&Path> for ConceptId {
    type Error = Error;

    fn try_from(path: &Path) -> Result<Self> {
        let mut segments: Vec<&str> = Vec::new();
        for component in path.components() {
            match component {
                Component::Prefix(_) | Component::RootDir | Component::ParentDir => {
                    return ValidationSnafu {
                        reason: format!(
                            "invalid concept path `{}`: must be relative with no `..`",
                            path.display()
                        ),
                    }
                    .fail();
                }
                Component::CurDir => continue,
                Component::Normal(segment) => {
                    segments.push(segment.to_str().with_context(|| ValidationSnafu {
                        reason: format!(
                            "invalid concept path `{}`: non-UTF-8 component",
                            path.display()
                        ),
                    })?)
                }
            }
        }
        if segments.is_empty() {
            return ValidationSnafu {
                reason: format!("invalid concept path `{}`: no segments", path.display()),
            }
            .fail();
        }
        ConceptId::from_str(&segments.join("/"))
    }
}

impl std::fmt::Display for ConceptId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_frontmatter() {
        let input = "---\ntype: Decision\ndescription: caching strategy\ntags:\n  - perf\n  - memory\n---\n# Body\n\nText here.\n";
        let concept = Concept::from_str(input).unwrap();
        assert_eq!(concept.concept_type(), Some("Decision"));
        assert_eq!(concept.description(), Some("caching strategy"));
        assert_eq!(concept.tags(), vec!["perf", "memory"]);
        assert_eq!(concept.body(), "# Body\n\nText here.\n");
        assert!(concept.is_okf_conformant());
    }

    #[test]
    fn tags_accept_single_string() {
        let concept = Concept::from_str("---\ntype: Note\ntags: solo\n---\nbody\n").unwrap();
        assert_eq!(concept.tags(), vec!["solo"]);
    }

    #[test]
    fn file_without_frontmatter_is_all_body_and_not_conformant() {
        let input = "# Just markdown\n\nNo frontmatter here.\n";
        let concept = Concept::from_str(input).unwrap();
        assert!(concept.frontmatter().is_empty());
        assert_eq!(concept.body(), input);
        assert!(!concept.is_okf_conformant());
    }

    #[test]
    fn empty_or_missing_type_is_not_conformant_but_unknown_type_is() {
        let no_type = Concept::from_str("---\ndescription: x\n---\nbody\n").unwrap();
        assert!(!no_type.is_okf_conformant());

        let empty_type = Concept::from_str("---\ntype: \"\"\n---\nbody\n").unwrap();
        assert!(!empty_type.is_okf_conformant());

        let unknown =
            Concept::from_str("---\ntype: Something Argosy Never Heard Of\n---\nbody\n").unwrap();
        assert!(unknown.is_okf_conformant());
    }

    #[test]
    fn unknown_keys_survive_round_trip() {
        let input = "---\ntype: Decision\ncustom_field: keepme\nnested:\n  deep: [1, 2]\n---\n# Body\nexact bytes\n";
        let concept = Concept::from_str(input).unwrap();

        // In-memory round trip.
        let serialized = concept.to_string();
        let reparsed = Concept::from_str(&serialized).unwrap();
        assert_eq!(reparsed.get_str("custom_field"), Some("keepme"));
        assert_eq!(reparsed.get("nested"), concept.get("nested"));
        assert_eq!(reparsed.body(), concept.body());

        // Through the filesystem.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("concept.md");
        concept.to_file(&path).unwrap();
        let reloaded = Concept::from_file(&path).unwrap();
        assert_eq!(reloaded, concept);
    }

    #[test]
    fn malformed_yaml_errors_naming_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.md");
        std::fs::write(&path, "---\n: [unclosed\n---\nbody\n").unwrap();

        let err = Concept::from_file(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("broken.md"),
            "error should name the file: {msg}"
        );
    }

    #[test]
    fn missing_file_io_error_names_the_path() {
        let err = Concept::from_file("/nonexistent-dir/nope.md").unwrap_err();
        assert!(err.to_string().contains("/nonexistent-dir/nope.md"));
    }

    #[test]
    fn unclosed_frontmatter_is_an_error_not_a_panic() {
        let err = Concept::from_str("---\ntype: X\nbody with no closer\n").unwrap_err();
        assert!(matches!(err, Error::MissingFrontmatter { .. }));
    }

    #[test]
    fn concept_id_round_trips() {
        let id: ConceptId = "document/decisions/2026-05-caching".parse().unwrap();
        assert_eq!(id.to_string(), "document/decisions/2026-05-caching");
        assert_eq!(
            id.to_relative_path(),
            PathBuf::from("document/decisions/2026-05-caching.md")
        );

        let from_path =
            ConceptId::try_from(Path::new("document/decisions/2026-05-caching.md")).unwrap();
        assert_eq!(from_path, id);
    }

    #[test]
    fn concept_id_rejects_parent_and_absolute_paths() {
        assert!("../escape".parse::<ConceptId>().is_err());
        assert!("a/../b".parse::<ConceptId>().is_err());
        assert!("/absolute/path".parse::<ConceptId>().is_err());
        assert!("a\\b".parse::<ConceptId>().is_err());
        assert!(ConceptId::try_from(Path::new("../x.md")).is_err());
        assert!(ConceptId::try_from(Path::new("/abs/x.md")).is_err());
    }

    #[test]
    fn concept_id_rejects_colons() {
        assert!("C:/evil".parse::<ConceptId>().is_err());
        assert!("a:b".parse::<ConceptId>().is_err());
    }

    #[test]
    fn utf8_bom_does_not_defeat_frontmatter_detection() {
        let concept = Concept::from_str("\u{feff}---\ntype: Decision\n---\nbody\n").unwrap();
        assert_eq!(concept.concept_type(), Some("Decision"));
        assert_eq!(concept.body(), "body\n");
        assert!(concept.is_okf_conformant());
    }

    #[test]
    fn non_mapping_frontmatter_is_malformed_frontmatter() {
        let err = Concept::from_str("---\n- one\n- two\n---\nbody\n").unwrap_err();
        assert!(matches!(err, Error::MissingFrontmatter { .. }));
    }

    #[test]
    fn tags_skips_non_string_sequence_items() {
        let concept =
            Concept::from_str("---\ntype: Note\ntags: [ok, 3, also-ok]\n---\nx\n").unwrap();
        assert_eq!(concept.tags(), vec!["ok", "also-ok"]);
    }

    #[test]
    fn new_rejects_infeasibly_deep_frontmatter_so_serialization_cannot_panic() {
        let mut inner = Value::Null;
        for i in 0..(MAX_FRONTMATTER_DEPTH + 10) {
            let mut m = Mapping::new();
            m.insert(Value::String(format!("k{i}")), inner);
            inner = Value::Mapping(m);
        }
        let deep = match inner {
            Value::Mapping(m) => m,
            _ => unreachable!(),
        };
        assert!(Concept::new(deep, String::new()).is_err());
    }

    #[test]
    fn parse_rejects_infeasibly_deep_frontmatter_so_serialization_cannot_panic() {
        // The same guard `Concept::new` enforces must apply to parsed input —
        // otherwise `to_string` would panic on a bundle-provided file.
        let mut yaml = "v".to_string();
        for _ in 0..(MAX_FRONTMATTER_DEPTH + 10) {
            yaml = format!("{{a: {yaml}}}");
        }
        let input = format!("---\nk: {yaml}\n---\nbody\n");
        let err = Concept::from_str(&input).unwrap_err();
        assert!(
            matches!(err, Error::Validation { .. }),
            "expected a validation error, got {err}"
        );

        // Just under the limit still parses and serializes fine.
        let mut yaml = "v".to_string();
        for _ in 0..50 {
            yaml = format!("{{a: {yaml}}}");
        }
        let concept = Concept::from_str(&format!("---\nk: {yaml}\n---\nbody\n")).unwrap();
        assert!(concept.to_string().contains("body"));
    }
}
