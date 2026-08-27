//! Opening and structural validation of an argosy bundle (spec §4, §11 step 2).
//!
//! [`Argosy::open`] answers "can I work with this bundle?" and errors only on
//! hard failures (no readable root, no parseable `Argosy Manifest` concept).
//! [`Argosy::validate`] answers "is this bundle conformant?" and, per OKF's
//! permissive conformance, reports everything it finds as [`Finding`]s rather
//! than rejecting over tolerable issues.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use semver::Version;
use snafu::{OptionExt, ResultExt};
use yaml_serde::{Mapping, Value};

use crate::concept::{Concept, ConceptId};
use crate::error::{Error, IoSnafu, MissingFieldSnafu, NotAnArgosySnafu, Result, ValidationSnafu};

/// A top-level bundle directory: one of the four reserved namespaces or a
/// producer-defined custom one (spec §4.3, `STR-7`–`STR-11`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Namespace {
    Document,
    Skill,
    Memory,
    Styleguide,
    /// Any other top-level directory (`STR-10`); the name is preserved.
    Custom(String),
}

impl Namespace {
    /// The four reserved top-level namespace directory names (`STR-7`).
    pub const RESERVED: [&'static str; 4] = ["document", "skill", "memory", "styleguide"];

    /// Filenames that are reserved everywhere in a bundle (§4.4): the root
    /// manifest plus OKF's listing and change-history files.
    pub const RESERVED_FILENAMES: [&'static str; 3] = ["argosy.md", "index.md", "log.md"];

    /// The directory name this namespace occupies under the bundle root.
    pub fn as_dir_name(&self) -> &str {
        match self {
            Self::Document => "document",
            Self::Skill => "skill",
            Self::Memory => "memory",
            Self::Styleguide => "styleguide",
            Self::Custom(name) => name,
        }
    }

    /// True iff this is one of the four reserved namespaces (`STR-7`).
    pub fn is_reserved(&self) -> bool {
        !matches!(self, Self::Custom(_))
    }

    /// Classifies a top-level directory name: reserved names map to their
    /// variants (`STR-7`), anything else to [`Namespace::Custom`] (`STR-10`).
    /// Validity of the name itself (a directory called `index.md`, say) is a
    /// separate question answered by validation (`STR-11`).
    pub fn from_dir_name(name: &str) -> Self {
        match name {
            "document" => Self::Document,
            "skill" => Self::Skill,
            "memory" => Self::Memory,
            "styleguide" => Self::Styleguide,
            other => Self::Custom(other.to_string()),
        }
    }

    /// `index.md` or `log.md` — OKF listing/history files that never count as
    /// concepts (spec §4.4).
    pub(crate) fn is_listing_file(name: &str) -> bool {
        name == "index.md" || name == "log.md"
    }
}

/// The parsed root `argosy.md` manifest (spec §4.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    name: String,
    argosy_version: Version,
    okf_version: Option<String>,
    description: Option<String>,
    /// Frontmatter keys not consumed above, retained untouched (`STR-6`).
    extra: Mapping,
}

impl Manifest {
    /// The `type` value a root `argosy.md` must declare (`STR-5`).
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
    /// missing/empty or `argosy_version` is missing/malformed.
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

    /// The argosy's identifying name (§4.2, required).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The version of this bundle's own content (§4.2, required).
    pub fn argosy_version(&self) -> &Version {
        &self.argosy_version
    }

    /// The OKF spec version the bundle targets (§4.2, `SHOULD`).
    pub fn okf_version(&self) -> Option<&str> {
        self.okf_version.as_deref()
    }

    /// A one-line summary of the bundle (§4.2, `SHOULD`).
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Unrecognized frontmatter keys, preserved in order (`STR-6`).
    pub fn extra(&self) -> &Mapping {
        &self.extra
    }
}

/// Extracts a string-ish scalar, tolerating unquoted YAML numbers/bools (an
/// unquoted `okf_version: 0.2` parses as a number, yet is read as `"0.2"`).
fn scalar_str(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// How serious a validation issue is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth surfacing; has no bearing on conformance (e.g. `STR-9`).
    Info,
    /// A `SHOULD` from the spec is unmet (e.g. missing `okf_version`).
    Warning,
    /// A `MUST`/`MUST NOT` from the spec is violated; blocks conformance.
    Error,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Info => "INFO",
            Self::Warning => "WARNING",
            Self::Error => "ERROR",
        })
    }
}

/// One issue found while validating a bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    /// The spec requirement ID this finding evidences (e.g. `STR-4`), if any.
    pub id: Option<&'static str>,
    /// Bundle-relative path concerned, when the finding is about a file.
    pub path: Option<PathBuf>,
    pub message: String,
}

impl Finding {
    pub(crate) fn new(
        severity: Severity,
        id: Option<&'static str>,
        path: Option<PathBuf>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            id,
            path,
            message: message.into(),
        }
    }
}

/// The outcome of structurally validating a bundle (spec §4, §11 step 2).
///
/// Scope boundary: validation checks the argosy structural requirements
/// (`STR-1`–`STR-11`), the generic OKF concept-level requirement that
/// every `.md` concept under the reserved namespaces carries a `type` (the
/// generic half of `DOC-1`/`MEM-1`/`STG-1`), and the `skill`/`styleguide`
/// namespace contracts (composed from [`crate::skill::validate`] and
/// [`crate::styleguide::validate`], also callable standalone via
/// [`Argosy::validate_skills`]/[`Argosy::validate_styleguide`]). Deeper OKF
/// conformance (link integrity, listing contents) is deliberately out of
/// scope.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationReport {
    findings: Vec<Finding>,
}

impl ValidationReport {
    pub(crate) fn push(&mut self, finding: Finding) {
        self.findings.push(finding);
    }

    /// All findings, in the deterministic order they were produced.
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// True iff no [`Severity::Error`] findings were produced.
    pub fn is_conformant(&self) -> bool {
        !self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    /// Findings with [`Severity::Error`].
    pub fn errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
    }

    /// Findings with [`Severity::Warning`].
    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
    }
}

impl fmt::Display for ValidationReport {
    /// One line per finding: `[ERROR STR-4] path/to/file.md: message`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for finding in &self.findings {
            let id = finding.id.map(|id| format!(" {id}")).unwrap_or_default();
            let path = finding
                .path
                .as_ref()
                .map(|p| format!("{}: ", p.display()))
                .unwrap_or_default();
            writeln!(f, "[{}{id}] {path}{}", finding.severity, finding.message)?;
        }
        Ok(())
    }
}

/// A bundle's directory entry, with its bundle-root-relative path.
pub(crate) struct WalkEntry {
    pub(crate) rel: PathBuf,
    pub(crate) is_dir: bool,
}

/// The outcome of a recursive walk: the entries found, plus every directory
/// that could not be read. Per-directory read failures are collected rather
/// than aborting the walk, so validation can report them precisely (with the
/// offending path) and still run its other checks.
#[derive(Default)]
pub(crate) struct WalkResult {
    pub(crate) entries: Vec<WalkEntry>,
    /// `(relative path, source)` of failed reads. An empty relative path
    /// means the walk root itself could not be read.
    pub(crate) unreadable: Vec<(PathBuf, std::io::Error)>,
}

/// Recursively collects every entry under `root.join(rel)`. `.argosy/` index
/// directories are skipped entirely: the index is a derivative artifact, not
/// bundle content (spec §3.1). Directory symlinks are not followed
/// (`file_type`, not `metadata`), so cycles are impossible.
pub(crate) fn walk_bundle(root: &Path, rel: &Path, walk: &mut WalkResult) {
    let rd = match fs::read_dir(root.join(rel)) {
        Ok(rd) => rd,
        Err(e) => {
            walk.unreadable.push((rel.to_path_buf(), e));
            return;
        }
    };
    let mut entries: Vec<fs::DirEntry> = rd
        .filter_map(|entry| match entry {
            Ok(entry) => Some(entry),
            Err(e) => {
                walk.unreadable.push((rel.to_path_buf(), e));
                None
            }
        })
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        let rel = rel.join(&name);
        let is_dir = match entry.file_type() {
            Ok(ty) => ty.is_dir(),
            Err(e) => {
                walk.unreadable.push((rel, e));
                continue;
            }
        };
        if is_dir && name == ".argosy" {
            continue;
        }
        walk.entries.push(WalkEntry {
            rel: rel.clone(),
            is_dir,
        });
        if is_dir {
            walk_bundle(root, &rel, walk);
        }
    }
}

/// Walks `root.join(rel)`, returning entries and read failures both sorted
/// deterministically by relative path.
pub(crate) fn sorted_walk(root: &Path, rel: &Path) -> WalkResult {
    let mut walk = WalkResult::default();
    walk_bundle(root, rel, &mut walk);
    walk.entries.sort_by(|a, b| a.rel.cmp(&b.rel));
    walk.unreadable.sort_by(|a, b| a.0.cmp(&b.0));
    walk
}

/// The requirement ID under which an unparseable or untyped concept under a
/// reserved namespace is reported. `document`/`memory`/`styleguide` have a
/// dedicated "must satisfy OKF concept conformance" requirement (`DOC-1`/
/// `MEM-1`/`STG-1`); `skill/` has none of its own (its specific `type: Skill`
/// contract is `SKL-3`, enforced by a later layer), so it falls back to the
/// bundle-wide OKF conformance requirement `STR-1`.
fn ns_conformance_id(ns: &str) -> &'static str {
    match ns {
        "document" => "DOC-1",
        "memory" => "MEM-1",
        "styleguide" => "STG-1",
        _ => "STR-1",
    }
}

/// A successfully opened argosy bundle (spec §4). Opening implies the hard
/// requirements hold; soft findings still require [`Argosy::validate`].
#[derive(Debug)]
pub struct Argosy {
    root: PathBuf,
    manifest: Manifest,
}

impl Argosy {
    /// Opens the bundle rooted at `path`. Errors only on hard failures: the
    /// root is not a readable directory, `argosy.md` is missing, or
    /// `argosy.md` is not a parseable `Argosy Manifest` concept (`STR-1`,
    /// `STR-2`, `STR-4`, `STR-5`). Everything validation can additionally
    /// surface comes back from [`Argosy::validate`], even for a bundle this
    /// call accepted.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let root = path.as_ref();

        let meta = fs::metadata(root).context(IoSnafu {
            path: root.to_path_buf(),
        })?;
        if !meta.is_dir() {
            return NotAnArgosySnafu {
                path: root.to_path_buf(),
                reason: "not a directory".to_string(),
            }
            .fail();
        }

        let manifest_path = root.join("argosy.md");
        if !manifest_path.is_file() {
            return NotAnArgosySnafu {
                path: root.to_path_buf(),
                reason: "no `argosy.md` manifest at the bundle root (STR-2)".to_string(),
            }
            .fail();
        }

        let concept = Concept::from_file(&manifest_path)?;
        let not_a_manifest = |reason: &str| {
            NotAnArgosySnafu {
                path: root.to_path_buf(),
                reason: reason.to_string(),
            }
            .fail()
        };
        if concept.frontmatter().is_empty() {
            return not_a_manifest("root `argosy.md` has no frontmatter (STR-4)");
        }
        if !concept
            .concept_type()
            .is_some_and(|t| t.trim() == Manifest::TYPE)
        {
            return not_a_manifest(&format!(
                "root `argosy.md` has type {:?}, expected {:?} (STR-5)",
                concept.concept_type().unwrap_or("<none>"),
                Manifest::TYPE,
            ));
        }

        // Declare manifest-field hard failures as `NotAnArgosy` too, so a
        // caller can classify "this path is not an argosy" by one variant.
        let manifest = Manifest::parse(&concept).map_err(|e| {
            NotAnArgosySnafu {
                path: root.to_path_buf(),
                reason: e.to_string(),
            }
            .build()
        })?;
        Ok(Self {
            root: root.to_path_buf(),
            manifest,
        })
    }

    /// Validates the bundle rooted at `path`. Works on any directory — even
    /// one [`Argosy::open`] would refuse — translating every problem into a
    /// [`Finding`] (an unreadable root becomes an `STR-1` error, for
    /// instance). Never rejects an argosy over a declared version (`NFR-5`).
    pub fn validate(path: impl AsRef<Path>) -> ValidationReport {
        let root = path.as_ref();
        let mut report = ValidationReport::default();

        let walk = sorted_walk(root, Path::new(""));
        let (unreadable, entries) = (walk.unreadable, walk.entries);
        // An unreadable root ends validation outright (`STR-1`); unreadable
        // directories deeper down become targeted errors and the rest of the
        // checks still run.
        if let Some((_, e)) = unreadable.iter().find(|(p, _)| p.as_os_str().is_empty()) {
            report.push(Finding::new(
                Severity::Error,
                Some("STR-1"),
                None,
                format!(
                    "bundle root `{}` is not a readable directory: {e}",
                    root.display()
                ),
            ));
            return report;
        }
        for (path, e) in &unreadable {
            report.push(Finding::new(
                Severity::Error,
                Some("STR-1"),
                Some(path.clone()),
                format!("directory could not be read: {e}"),
            ));
        }

        Self::validate_manifest(root, &entries, &mut report);
        Self::validate_namespaces(&entries, &mut report);
        Self::validate_concepts(root, &entries, &mut report);
        // Namespace contracts (spec §5.2, §5.4). Unreadable directories are
        // already reported above as `STR-1`; these validators skip them.
        for finding in crate::skill::validate(root) {
            report.push(finding);
        }
        for finding in crate::styleguide::validate(root) {
            report.push(finding);
        }
        report
    }

    /// Root manifest checks: `STR-2`/`STR-3`/`STR-4`/`STR-5` plus the §4.2
    /// `SHOULD` fields as warnings.
    fn validate_manifest(root: &Path, entries: &[WalkEntry], report: &mut ValidationReport) {
        // STR-3 / STR-11: `argosy.md` below the root (as a concept, a nested
        // bundle, or anything else) is forbidden.
        for entry in entries
            .iter()
            .filter(|e| !e.is_dir && e.rel.file_name() == Some(std::ffi::OsStr::new("argosy.md")))
        {
            if entry.rel != Path::new("argosy.md") {
                report.push(Finding::new(
                    Severity::Error,
                    Some("STR-3"),
                    Some(entry.rel.clone()),
                    "`argosy.md` is reserved for the root manifest and must not \
                     appear anywhere below it (also `STR-4`/`STR-11`)",
                ));
            }
        }

        let manifest_rel = PathBuf::from("argosy.md");
        let present = entries.iter().any(|e| !e.is_dir && e.rel == manifest_rel);
        if !present {
            report.push(Finding::new(
                Severity::Error,
                Some("STR-2"),
                None,
                "no `argosy.md` manifest at the bundle root",
            ));
            return;
        }

        let concept = match Concept::from_file(root.join(&manifest_rel)) {
            Ok(concept) => concept,
            Err(e) => {
                report.push(Finding::new(
                    Severity::Error,
                    Some("STR-4"),
                    Some(manifest_rel),
                    format!("root `argosy.md` failed to parse as a concept: {e}"),
                ));
                return;
            }
        };

        if concept.frontmatter().is_empty() {
            report.push(Finding::new(
                Severity::Error,
                Some("STR-4"),
                Some(manifest_rel),
                "root `argosy.md` has no frontmatter, so it is not an OKF concept",
            ));
            return;
        }

        if !concept
            .concept_type()
            .is_some_and(|t| t.trim() == Manifest::TYPE)
        {
            report.push(Finding::new(
                Severity::Error,
                Some("STR-5"),
                Some(manifest_rel.clone()),
                format!(
                    "root `argosy.md` has type {:?}, expected {:?}",
                    concept.concept_type().unwrap_or("<none>"),
                    Manifest::TYPE,
                ),
            ));
        }

        // §4.2 fields: `name` and `argosy_version` are MUST (STR-5);
        // `okf_version` and `description` are SHOULD (Warning when absent).
        if concept.get_str("name").is_none_or(|n| n.trim().is_empty()) {
            report.push(Finding::new(
                Severity::Error,
                Some("STR-5"),
                Some(manifest_rel.clone()),
                "manifest must declare a non-empty `name` field (§4.2)",
            ));
        }
        match concept.get("argosy_version").and_then(scalar_str) {
            None => report.push(Finding::new(
                Severity::Error,
                Some("STR-5"),
                Some(manifest_rel.clone()),
                "manifest must declare an `argosy_version` field (§4.2)",
            )),
            Some(v) if Version::parse(v.trim()).is_err() => {
                report.push(Finding::new(
                    Severity::Error,
                    Some("STR-5"),
                    Some(manifest_rel.clone()),
                    format!("manifest `argosy_version` `{v}` is not valid semver"),
                ));
            }
            Some(_) => {}
        }
        if concept.get("okf_version").is_none() {
            report.push(Finding::new(
                Severity::Warning,
                None,
                Some(manifest_rel.clone()),
                "manifest SHOULD declare the `okf_version` it targets (§4.2)",
            ));
        }
        if concept.get_str("description").is_none_or(str::is_empty) {
            report.push(Finding::new(
                Severity::Warning,
                None,
                Some(manifest_rel),
                "manifest SHOULD declare a `description` (§4.2)",
            ));
        }
    }

    /// Top-level layout checks: `STR-7` (reserved names are directories, not
    /// files), `STR-9` (`memory/` noted), and `STR-11` (reserved filenames
    /// used as directory names). Custom directories need no finding (`STR-10`).
    fn validate_namespaces(entries: &[WalkEntry], report: &mut ValidationReport) {
        for entry in entries {
            let mut components = entry.rel.components();
            let first = components.next();
            let depth_one = components.next().is_none();

            if entry.is_dir {
                let name = entry.rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if Namespace::RESERVED_FILENAMES.contains(&name) {
                    report.push(Finding::new(
                        Severity::Error,
                        Some("STR-11"),
                        Some(entry.rel.clone()),
                        format!(
                            "directory `{name}/` collides with the reserved filename `{name}` (§4.4)"
                        ),
                    ));
                }
            } else if depth_one && let Some(std::path::Component::Normal(name)) = first {
                let name = name.to_string_lossy();
                if Namespace::RESERVED.contains(&name.as_ref()) {
                    report.push(Finding::new(
                        Severity::Error,
                        Some("STR-7"),
                        Some(entry.rel.clone()),
                        format!(
                            "`{name}` is a reserved namespace name and must appear as a \
                             top-level directory, not a file"
                        ),
                    ));
                }
            }
        }

        let memory_present = entries
            .iter()
            .any(|e| e.is_dir && e.rel == Path::new("memory"));
        if memory_present {
            report.push(Finding::new(
                Severity::Info,
                Some("STR-9"),
                Some(PathBuf::from("memory")),
                "`memory/` present: fine for a local argosy; on imported argosys \
                 its content is read-only (MUL-3, enforced by the import layer)",
            ));
        }
    }

    /// Generic OKF concept conformance (`type` present) for every `.md` under
    /// the four reserved namespaces — the generic half of `DOC-1`/`MEM-1`/
    /// `STG-1`. OKF listing/history files (`index.md`/`log.md`) and nested
    /// `argosy.md` (already reported under `STR-3`) are skipped; namespace
    /// -specific contracts (`SKL-*`, `STG-2`–`STG-4`) come from
    /// [`crate::skill::validate`]/[`crate::styleguide::validate`].
    fn validate_concepts(root: &Path, entries: &[WalkEntry], report: &mut ValidationReport) {
        for entry in entries {
            if entry.is_dir || entry.rel.extension() != Some(std::ffi::OsStr::new("md")) {
                continue;
            }
            let Some(std::path::Component::Normal(ns)) = entry.rel.components().next() else {
                continue;
            };
            let ns = ns.to_string_lossy();
            if !Namespace::RESERVED.contains(&ns.as_ref()) {
                continue;
            }
            let name = entry.rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if Namespace::is_listing_file(name) || name == "argosy.md" {
                continue;
            }
            let id = ns_conformance_id(&ns);
            match Concept::from_file(root.join(&entry.rel)) {
                Err(e) => report.push(Finding::new(
                    Severity::Error,
                    Some(id),
                    Some(entry.rel.clone()),
                    format!("concept failed to parse: {e}"),
                )),
                Ok(concept) if !concept.is_okf_conformant() => report.push(Finding::new(
                    Severity::Error,
                    Some(id),
                    Some(entry.rel.clone()),
                    "concept has no frontmatter `type` (OKF concept conformance)",
                )),
                Ok(_) => {}
            }
        }
    }

    /// The bundle root this handle was opened from.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The parsed root manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The directory of `namespace`, if it exists (`STR-8`: absent namespaces
    /// are fine).
    pub fn namespace_dir(&self, namespace: &Namespace) -> Option<PathBuf> {
        let dir = self.root.join(namespace.as_dir_name());
        dir.is_dir().then_some(dir)
    }

    /// All namespaces actually present: reserved first (in [`Namespace::RESERVED`]
    /// order), then custom directories sorted by name.
    pub fn namespaces_present(&self) -> Vec<Namespace> {
        let mut present: Vec<Namespace> = Namespace::RESERVED
            .iter()
            .filter(|name| self.root.join(name).is_dir())
            .map(|name| Namespace::from_dir_name(name))
            .collect();

        // A read error just means the custom list is empty — the reserved
        // namespaces above are still authoritative.
        let mut custom: Vec<String> = fs::read_dir(&self.root)
            .map(|rd| {
                rd.filter_map(|entry| entry.ok())
                    .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
                    .filter_map(|entry| entry.file_name().into_string().ok())
                    .filter(|name| {
                        name != ".argosy" && !Namespace::RESERVED.contains(&name.as_str())
                    })
                    .collect()
            })
            .unwrap_or_default();
        custom.sort();
        present.extend(custom.into_iter().map(|n| Namespace::from_dir_name(&n)));
        present
    }

    /// Every concept under `namespace`, as `(id, concept)` pairs sorted by
    /// [`ConceptId`]. OKF listing/history files (`index.md`/`log.md`, §4.4)
    /// are excluded, as is the `.argosy/` index directory. An absent
    /// namespace yields an empty vec.
    pub fn concepts(&self, namespace: &Namespace) -> Result<Vec<(ConceptId, Concept)>> {
        let Some(dir) = self.namespace_dir(namespace) else {
            return Ok(Vec::new());
        };
        let rel = Path::new(namespace.as_dir_name());
        let walk = sorted_walk(&self.root, rel);
        // Any read failure under this namespace is a hard error for listing.
        if let Some((path, source)) = walk.unreadable.into_iter().next() {
            let path = if path.as_os_str().is_empty() {
                dir
            } else {
                self.root.join(path)
            };
            return Err(Error::Io { path, source });
        }
        let entries = walk.entries;

        let mut out = Vec::new();
        for entry in entries
            .iter()
            .filter(|e| !e.is_dir && e.rel.extension() == Some(std::ffi::OsStr::new("md")))
        {
            let name = entry.rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if Namespace::is_listing_file(name) {
                continue;
            }
            let id = ConceptId::try_from(entry.rel.as_path())?;
            let concept = Concept::from_file(self.root.join(&entry.rel))?;
            out.push((id, concept));
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn error_ids(report: &ValidationReport) -> Vec<Option<&'static str>> {
        report.errors().map(|f| f.id).collect()
    }

    #[test]
    fn open_valid_fixture_reads_manifest_fields() {
        let argosy = Argosy::open(fixture("valid-acme-billing")).unwrap();
        let manifest = argosy.manifest();
        assert_eq!(manifest.name(), "acme-billing");
        assert_eq!(manifest.argosy_version(), &Version::new(0, 3, 1));
        assert_eq!(manifest.okf_version(), Some("0.2"));
        assert_eq!(
            manifest.description(),
            Some("Knowledge, skills, and memory for the ACME billing service.")
        );
    }

    #[test]
    fn open_valid_fixture_lists_all_reserved_and_custom_namespaces() {
        let argosy = Argosy::open(fixture("valid-acme-billing")).unwrap();
        let present = argosy.namespaces_present();
        assert_eq!(
            present,
            vec![
                Namespace::Document,
                Namespace::Skill,
                Namespace::Memory,
                Namespace::Styleguide,
                Namespace::Custom("roadmap".to_string()),
            ]
        );
        // Reserved namespaces map back to directories; absent handling works.
        assert!(argosy.namespace_dir(&Namespace::Skill).is_some());
        assert!(
            argosy
                .namespace_dir(&Namespace::Custom("nope".to_string()))
                .is_none()
        );
    }

    #[test]
    fn valid_fixture_is_conformant_with_no_errors() {
        let report = Argosy::validate(fixture("valid-acme-billing"));
        assert!(report.is_conformant());
        assert_eq!(report.errors().count(), 0);
        // `memory/` presence is surfaced as an STR-9 info note, not an issue.
        let infos: Vec<_> = report
            .findings()
            .iter()
            .filter(|f| f.severity == Severity::Info)
            .collect();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].id, Some("STR-9"));
    }

    #[test]
    fn unknown_manifest_keys_are_retained_without_findings() {
        let fixture = fixture("valid-acme-billing");
        let argosy = Argosy::open(&fixture).unwrap();
        // STR-6: unknown keys parse fine and are retained (`x-team`, `generated`).
        assert_eq!(
            argosy
                .manifest()
                .extra()
                .get("x-team")
                .and_then(Value::as_str),
            Some("payments")
        );
        assert!(argosy.manifest().extra().contains_key("generated"));
        // Known-but-unconsumed OKF fields stay too; consumed ones don't.
        assert!(argosy.manifest().extra().contains_key("tags"));
        assert!(!argosy.manifest().extra().contains_key("name"));

        let report = Argosy::validate(&fixture);
        assert!(
            report
                .findings()
                .iter()
                .all(|f| !f.message.contains("x-team"))
        );
    }

    #[test]
    fn concepts_skill_is_sorted_and_excludes_listing_files() {
        let argosy = Argosy::open(fixture("valid-acme-billing")).unwrap();
        let skills = argosy.concepts(&Namespace::Skill).unwrap();
        let ids: Vec<_> = skills.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "skill/reconcile-ledger",
                "skill/rotate-api-keys/references/checklist",
                "skill/rotate-api-keys/rotate-api-keys",
            ]
        );
        let sorted = ids.is_sorted();
        assert!(sorted);
    }

    #[test]
    fn concepts_of_absent_namespace_is_empty_not_error() {
        let argosy = Argosy::open(fixture("valid-acme-billing")).unwrap();
        assert!(
            argosy
                .concepts(&Namespace::Custom("absent".to_string()))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn dot_argosy_index_directory_is_ignored_everywhere() {
        let fixture = fixture("valid-acme-billing");
        let argosy = Argosy::open(&fixture).unwrap();
        assert!(
            !argosy
                .namespaces_present()
                .contains(&Namespace::Custom(".argosy".to_string()))
        );
        let report = Argosy::validate(&fixture);
        assert!(report.findings().iter().all(|f| {
            !f.path
                .as_ref()
                .is_some_and(|p| p.to_string_lossy().contains(".argosy"))
        }));
    }

    #[test]
    fn minimal_manifest_warns_about_should_fields_but_still_opens() {
        let fixture = fixture("minimal-manifest");
        Argosy::open(&fixture).unwrap();
        let report = Argosy::validate(&fixture);
        assert!(report.is_conformant());
        let warnings: Vec<_> = report.warnings().collect();
        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().any(|f| f.message.contains("okf_version")));
        assert!(warnings.iter().any(|f| f.message.contains("description")));
    }

    #[test]
    fn missing_manifest_is_str2_error_and_open_fails() {
        let fixture = fixture("missing-manifest");
        let report = Argosy::validate(&fixture);
        assert_eq!(error_ids(&report), vec![Some("STR-2")]);
        assert!(Argosy::open(&fixture).is_err());
    }

    #[test]
    fn wrong_manifest_type_is_str5_error_and_open_fails() {
        let fixture = fixture("wrong-manifest-type");
        let report = Argosy::validate(&fixture);
        assert!(error_ids(&report).contains(&Some("STR-5")));
        assert!(Argosy::open(&fixture).is_err());
    }

    #[test]
    fn manifest_without_frontmatter_is_str4_error_and_open_fails() {
        let fixture = fixture("manifest-no-frontmatter");
        let report = Argosy::validate(&fixture);
        assert_eq!(error_ids(&report), vec![Some("STR-4")]);
        assert!(Argosy::open(&fixture).is_err());
    }

    #[test]
    fn nested_argosy_manifest_is_str3_error() {
        let report = Argosy::validate(fixture("nested-argosy"));
        assert_eq!(error_ids(&report), vec![Some("STR-3")]);
    }

    #[test]
    fn argosy_md_used_as_ordinary_concept_is_str3_error() {
        let report = Argosy::validate(fixture("argosy-as-concept"));
        assert_eq!(error_ids(&report), vec![Some("STR-3")]);
    }

    #[test]
    fn malformed_semver_is_str5_error_and_open_fails() {
        let fixture = fixture("bad-semver");
        let report = Argosy::validate(&fixture);
        assert!(error_ids(&report).contains(&Some("STR-5")));
        assert!(Argosy::open(&fixture).is_err());
    }

    #[test]
    fn reserved_namespace_name_as_toplevel_file_is_str7_error() {
        let report = Argosy::validate(fixture("reserved-as-file-document"));
        assert_eq!(error_ids(&report), vec![Some("STR-7")]);
    }

    #[test]
    fn reserved_filename_as_directory_is_str11_error() {
        let report = Argosy::validate(fixture("index-md-as-dir"));
        assert_eq!(error_ids(&report), vec![Some("STR-11")]);
    }

    #[test]
    fn untyped_document_concept_is_doc1_error() {
        let report = Argosy::validate(fixture("untyped-concept"));
        assert_eq!(error_ids(&report), vec![Some("DOC-1")]);
    }

    #[test]
    fn validate_on_non_directory_root_is_str1_error() {
        let file = fixture("missing-manifest").join("document/note.md");
        let report = Argosy::validate(&file);
        assert_eq!(error_ids(&report), vec![Some("STR-1")]);

        let missing = fixture("does-not-exist");
        let report = Argosy::validate(&missing);
        assert_eq!(error_ids(&report), vec![Some("STR-1")]);
        assert!(Argosy::open(&missing).is_err());
    }

    #[test]
    fn report_display_renders_one_finding_per_line_with_id_and_path() {
        let rendered = Argosy::validate(fixture("untyped-concept")).to_string();
        let line = rendered.trim_end();
        assert!(
            line.starts_with("[ERROR DOC-1] document/untyped.md: "),
            "unexpected rendering: {line}"
        );
        assert_eq!(rendered.lines().count(), 1);
    }

    #[test]
    fn namespace_names_round_trip() {
        for name in Namespace::RESERVED {
            let ns = Namespace::from_dir_name(name);
            assert!(ns.is_reserved());
            assert_eq!(ns.as_dir_name(), name);
        }
        let custom = Namespace::from_dir_name("roadmap");
        assert_eq!(custom, Namespace::Custom("roadmap".to_string()));
        assert!(!custom.is_reserved());
        assert_eq!(custom.as_dir_name(), "roadmap");
    }

    #[test]
    fn manifest_parse_rejects_missing_name_and_version() {
        let concept = Concept::from_str("---\ntype: Argosy Manifest\n---\nbody\n").unwrap();
        assert!(Manifest::parse(&concept).is_err());
        let concept = Concept::from_str(
            "---\ntype: Argosy Manifest\nname: x\nargosy_version: nope\n---\nbody\n",
        )
        .unwrap();
        assert!(Manifest::parse(&concept).is_err());
    }

    #[test]
    fn open_reports_bad_manifest_fields_as_not_an_argosy() {
        let err = Argosy::open(fixture("bad-semver")).unwrap_err();
        assert!(
            matches!(err, Error::NotAnArgosy { .. }),
            "expected NotAnArgosy, got {err}"
        );
    }

    /// A permission-denied subdirectory must not abort validation: the rest
    /// of the checks still run, and the finding names the offending path.
    #[cfg(unix)]
    #[test]
    fn unreadable_subdirectory_is_a_targeted_str1_not_a_root_failure() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("argosy.md"),
            "---\ntype: Argosy Manifest\nname: t\nargosy_version: \"1.0.0\"\n\
             okf_version: \"0.2\"\ndescription: t\n---\n# t\n",
        )
        .unwrap();
        let secret = root.join("document/secret");
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::write(secret.join("hidden.md"), "# hidden\n").unwrap();
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();

        let report = Argosy::validate(root);
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o755)).unwrap();

        let str1: Vec<_> = report.errors().filter(|f| f.id == Some("STR-1")).collect();
        assert_eq!(str1.len(), 1);
        assert_eq!(str1[0].path, Some(PathBuf::from("document/secret")));
        assert!(
            str1[0].message.contains("could not be read"),
            "unexpected message: {}",
            str1[0].message
        );
    }
}
