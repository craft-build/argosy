//! The opened bundle: structural validation and concept access.

use std::fs;
use std::path::{Path, PathBuf};

use semver::Version;

use crate::concept::{Concept, ConceptId};
use crate::error::{Error, IoSnafu, NotAnArgosySnafu, Result};
use snafu::ResultExt;

use super::manifest::{Manifest, is_real_dir, is_safe_bundle_name, is_safe_component, scalar_str};
use super::namespace::Namespace;
use super::validation::{Finding, Severity, ValidationReport};
use super::walk::{WalkEntry, sorted_walk};

/// The requirement ID under which an unparseable or untyped concept under a
/// reserved namespace is reported. `skill/` has no such requirement of its
/// own, so findings there carry no ID rather than a misleading root-structure
/// one.
fn ns_conformance_id(ns: &str) -> Option<&'static str> {
    match ns {
        "document" => Some("DOC-1"),
        "memory" => Some("MEM-1"),
        "styleguide" => Some("STG-1"),
        _ => None,
    }
}

/// A successfully opened argosy bundle. Opening implies the hard
/// requirements hold; soft findings still require [`Argosy::validate`].
#[derive(Debug)]
pub struct Argosy {
    root: PathBuf,
    manifest: Manifest,
}

impl Argosy {
    /// Opens the bundle rooted at `path`. Errors only on hard failures:
    /// unreadable root, missing or unparseable `argosy.md`, wrong manifest
    /// type, or invalid manifest fields. Everything else surfaces via
    /// [`Argosy::validate`].
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

        // Manifest-field errors (missing `name`, malformed `argosy_version`)
        // hard-fail here as `NotAnArgosy` so callers classify "not an argosy"
        // by one variant; the same problems still surface via `validate()`.
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
    /// [`Finding`] (an unreadable root becomes an error, for instance).
    /// Never rejects an argosy over a declared version.
    pub fn validate(path: impl AsRef<Path>) -> ValidationReport {
        let root = path.as_ref();
        let mut report = ValidationReport::default();

        let walk = sorted_walk(root, Path::new(""));
        let (unreadable, entries) = (walk.unreadable, walk.entries);
        // An unreadable root ends validation outright; unreadable
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
        // Namespace contracts. Unreadable directories are already reported
        // above; these validators skip them.
        for finding in crate::skill::validate(root) {
            report.push(finding);
        }
        for finding in crate::styleguide::validate(root) {
            report.push(finding);
        }
        report
    }

    /// Root manifest checks: presence, type, parseability, and field
    /// validity, plus `SHOULD` fields as warnings.
    fn validate_manifest(root: &Path, entries: &[WalkEntry], report: &mut ValidationReport) {
        // `argosy.md` below the root is forbidden, whatever it contains.
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

        // `name` and `argosy_version` are required; `okf_version` and
        // `description` are recommended (Warning when absent).
        if concept.get_str("name").is_none_or(|n| n.trim().is_empty()) {
            report.push(Finding::new(
                Severity::Error,
                Some("STR-5"),
                Some(manifest_rel.clone()),
                "manifest must declare a non-empty `name` field (§4.2)",
            ));
        } else if let Some(name) = concept.get_str("name").map(str::trim)
            && !is_safe_bundle_name(name)
        {
            // `Manifest::parse` refuses the same names, so `validate` must
            // report them — a conformant bundle always opens.
            report.push(Finding::new(
                Severity::Error,
                Some("STR-5"),
                Some(manifest_rel.clone()),
                format!(
                    "manifest `name` `{name}` is outside the URI charset [A-Za-z0-9._-]; \
                     the name appears in argosy:// URIs, so the argosy cannot be opened \
                     as named (§4.2)"
                ),
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
        // The SHOULD-level fields have no requirement ID of their own; the
        // finding labels the section so consumers can still trace them.
        if concept.get("okf_version").is_none() {
            report.push(Finding::new(
                Severity::Warning,
                Some("§4.2"),
                Some(manifest_rel.clone()),
                "manifest SHOULD declare the `okf_version` it targets (§4.2)",
            ));
        }
        if concept.get_str("description").is_none_or(str::is_empty) {
            report.push(Finding::new(
                Severity::Warning,
                Some("§4.2"),
                Some(manifest_rel),
                "manifest SHOULD declare a `description` (§4.2)",
            ));
        }
    }

    /// Top-level layout checks: reserved names must be directories (not
    /// files), `memory/` presence is noted, and reserved filenames must not
    /// be used as directory names.
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
    /// the four reserved namespaces. OKF listing/history files and nested
    /// `argosy.md` are skipped; namespace-specific contracts come from
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
                    id,
                    Some(entry.rel.clone()),
                    format!("concept failed to parse: {e}"),
                )),
                Ok(concept) if !concept.is_okf_conformant() => report.push(Finding::new(
                    Severity::Error,
                    id,
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

    /// The directory of `namespace`, if it exists; absent namespaces are
    /// fine. Only real directories count: a symlinked namespace would let
    /// reads escape the bundle root, and an unsafe [`Namespace::Custom`]
    /// name is refused rather than joined.
    pub fn namespace_dir(&self, namespace: &Namespace) -> Option<PathBuf> {
        if let Namespace::Custom(name) = namespace
            && !is_safe_component(name)
        {
            return None;
        }
        let dir = self.root.join(namespace.as_dir_name());
        is_real_dir(&dir).then_some(dir)
    }

    /// All namespaces actually present: reserved first (in [`Namespace::RESERVED`]
    /// order), then custom directories sorted by name.
    pub fn namespaces_present(&self) -> Vec<Namespace> {
        let mut present: Vec<Namespace> = Namespace::RESERVED
            .iter()
            .filter(|name| is_real_dir(&self.root.join(name)))
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
    /// [`ConceptId`]. OKF listing/history files (`index.md`/`log.md`) are
    /// excluded, as is the `.argosy/` index directory. An absent
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
