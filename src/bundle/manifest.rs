//! The `argosy.md` manifest: parsing, field access, and name safety.

use std::fs;
use std::path::Path;

use semver::Version;
use snafu::{OptionExt, ensure};
use yaml_serde::{Mapping, Value};

use crate::concept::Concept;
use crate::error::{MissingFieldSnafu, Result, ValidationSnafu};

/// The parsed root `argosy.md` manifest.
#[derive(Debug, Clone, PartialEq)]

pub struct Manifest {
    name: String,
    argosy_version: Version,
    okf_version: Option<String>,
    description: Option<String>,
    /// Frontmatter keys not consumed above, retained untouched.
    extra: Mapping,
}

impl Manifest {
    /// The `type` value a root `argosy.md` must declare.
    pub const TYPE: &'static str = "Argosy Manifest";

    /// Frontmatter keys the manifest consumes; everything else lands in
    /// [`Manifest::extra`].
    const KNOWN_KEYS: [&'static str; 5] = [
        "type",
        "name",
        "argosy_version",
        "okf_version",
        "description",
    ];

    /// Builds a manifest from the parsed root concept. Fails if `name` is
    /// missing/empty or outside the URI charset `[A-Za-z0-9._-]` (the name
    /// appears in `argosy://` URIs — a name the resolver would reject must
    /// fail at open, not at first use), or if `argosy_version` is
    /// missing/malformed.
    pub fn parse(concept: &Concept) -> Result<Self> {
        let name = concept
            .get_str("name")
            .filter(|n| !n.trim().is_empty())
            .with_context(|| MissingFieldSnafu {
                field: "name",
                path: None,
            })?
            .trim()
            .to_string();
        ensure!(
            is_safe_bundle_name(&name),
            ValidationSnafu {
                reason: format!(
                    "manifest `name` `{name}` is outside the URI charset [A-Za-z0-9._-]; \
                     the name appears in argosy:// URIs, so rename the argosy before using it"
                )
            }
        );

        let raw_version = concept
            .get("argosy_version")
            .and_then(scalar_str)
            .filter(|v| !v.trim().is_empty())
            .with_context(|| MissingFieldSnafu {
                field: "argosy_version",
                path: None,
            })?;
        let argosy_version = Version::parse(raw_version.trim()).map_err(|e| {
            ValidationSnafu {
                reason: format!("invalid `argosy_version` `{raw_version}`: {e}"),
            }
            .build()
        })?;

        let okf_version = concept.get("okf_version").and_then(scalar_str);
        let description = concept.get_str("description").map(str::to_string);

        let extra = concept
            .frontmatter()
            .iter()
            .filter(|(k, _)| k.as_str().is_none_or(|k| !Self::KNOWN_KEYS.contains(&k)))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        Ok(Self {
            name,
            argosy_version,
            okf_version,
            description,
            extra,
        })
    }

    /// The argosy's identifying name (required).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The version of this bundle's own content (required).
    pub fn argosy_version(&self) -> &Version {
        &self.argosy_version
    }

    /// The OKF spec version the bundle targets.
    pub fn okf_version(&self) -> Option<&str> {
        self.okf_version.as_deref()
    }

    /// A one-line summary of the bundle.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Unrecognized frontmatter keys, preserved in order.
    pub fn extra(&self) -> &Mapping {
        &self.extra
    }
}

/// True iff `name` is a single safe path component — never empty, `.`/`..`,
/// or containing separators — so joining it under the bundle root cannot
/// traverse out of the bundle.
pub(super) fn is_safe_component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', ':'])
}

/// True iff `name` is a usable manifest/checkout name: the URI charset
/// `[A-Za-z0-9._-]` (the manifest name appears in `argosy://` URIs), never
/// empty or a `.`/`..` component.
pub(crate) fn is_safe_bundle_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// True iff `path` is a real directory. `symlink_metadata` (not `is_dir`) is
/// used so a symlink pointing outside the bundle is refused — entering a
/// namespace through a symlink would bypass the walk's no-follow policy and
/// let bundle content live outside the bundle root.
pub(crate) fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir())
}

/// True iff `path` is a real file. `symlink_metadata` (not `is_file`) is
/// used so a concept can never be read through a symlink — the walk's
/// no-follow policy must hold for files too, or a symlinked concept would
/// let bundle content live outside (or outside content leak into) the
/// bundle root.
pub(crate) fn is_real_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// Extracts a string-ish scalar, tolerating unquoted YAML numbers/bools (an
/// unquoted `okf_version: 0.2` parses as a number, yet is read as `"0.2"`).
pub(super) fn scalar_str(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}
