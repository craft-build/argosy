//! The write surface of the local argosy: concept writes, deletes, and the
//! memory-to-distributable promotion pathway. `Argosy` is read-only, so
//! writing to an import is unrepresentable; custom namespaces are refused.
//! No index-invalidation hooks — reconcile discovers changes by hash.
//! Promotion is mechanical; `memory/` stays out of packaging.

use std::fs;
use std::ops::Deref;
use std::path::{Component, Path, PathBuf};

use snafu::{OptionExt, ResultExt, ensure};
use yaml_serde::{Mapping, Value};

use crate::bundle::{Argosy, Manifest, Namespace, Severity};
use crate::concept::{Concept, ConceptId};
use crate::error::{
    ConceptExistsSnafu, ConceptNotFoundSnafu, IoSnafu, NamespaceContractViolationSnafu,
    ReservedFilenameSnafu, Result, ValidationSnafu,
};

/// The requirement ID under which a missing frontmatter `type` is reported
/// when refusing a write — mirroring `bundle::ns_conformance_id`, except the
/// generic OKF requirement has no ID of its own for `skill`.
fn conformance_requirement(namespace: &Namespace) -> &'static str {
    match namespace {
        Namespace::Document => "DOC-1",
        Namespace::Memory => "MEM-1",
        Namespace::Styleguide => "STG-1",
        _ => "OKF concept conformance",
    }
}

/// True iff `inner` — a path relative to `skill/` — names a skill
/// entry-point position: file form `foo.md` at the top level, or directory
/// form `foo/foo.md`. Everything else under `skill/` is a plain concept.
fn is_skill_entry_point(inner: &Path) -> bool {
    let components: Vec<&str> = inner
        .components()
        .map(|c| match c {
            Component::Normal(s) => s.to_str().unwrap_or(""),
            _ => "",
        })
        .collect();
    match components.as_slice() {
        [file] => file.ends_with(".md"),
        [dir, file] if !dir.is_empty() => *file == format!("{dir}.md"),
        _ => false,
    }
}

/// The contract violation that keeps a `concept` out of `namespace` at
/// bundle-relative path `rel`: OKF conformance everywhere, the rule
/// contract under `styleguide/`, and the entry-point contract under
/// `skill/` when the write lands at an entry-point position.
fn contract_violation(
    namespace: &Namespace,
    rel: &Path,
    concept: &Concept,
) -> Option<crate::error::Error> {
    if !concept.is_okf_conformant() {
        return Some(
            NamespaceContractViolationSnafu {
                requirement: conformance_requirement(namespace),
                detail: "concept has no frontmatter `type` (OKF concept conformance)".to_string(),
            }
            .build(),
        );
    }

    match namespace {
        Namespace::Styleguide => {
            if concept.concept_type().map(str::trim) != Some(crate::styleguide::TYPE) {
                return Some(
                    NamespaceContractViolationSnafu {
                        requirement: "STG-2",
                        detail: format!(
                            "styleguide rule must have `type: Styleguide Rule`, got `{}`",
                            concept.concept_type().unwrap_or("<none>")
                        ),
                    }
                    .build(),
                );
            }
            if concept.description().is_none_or(|d| d.trim().is_empty()) {
                return Some(
                    NamespaceContractViolationSnafu {
                        requirement: "STG-3",
                        detail: "styleguide rule must set a non-empty `description`; it is the \
                                 text consumers embed and match against"
                            .to_string(),
                    }
                    .build(),
                );
            }
            None
        }
        Namespace::Skill if is_skill_entry_point(rel.strip_prefix("skill").unwrap_or(rel)) => {
            // OKF conformance was checked above, so the entry-point
            // checks' untyped-concept skip path (delegation to the
            // structural validator) can't hide a problem here.
            let mut findings = Vec::new();
            crate::skill::entry_point_findings(rel, concept, &mut findings);
            findings
                .into_iter()
                .find(|f| f.severity == Severity::Error)
                .map(|f| {
                    NamespaceContractViolationSnafu {
                        requirement: f.id.unwrap_or("SKL-1"),
                        detail: f.message,
                    }
                    .build()
                })
        }
        _ => None,
    }
}

/// The local, writable argosy — the only type with write APIs (see module
/// docs). Derefs to [`Argosy`], so every read API is available
/// unchanged.
#[derive(Debug)]
pub struct LocalArgosy(Argosy);

impl Deref for LocalArgosy {
    type Target = Argosy;

    fn deref(&self) -> &Argosy {
        &self.0
    }
}

/// What a promotion turns a memory concept into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionTarget {
    /// A prose concept under `document/` (the default target).
    Document,
    /// A `type: Styleguide Rule` concept under `styleguide/`, which must
    /// satisfy the styleguide namespace contract like any other rule.
    StyleguideRule,
}

/// The outcome of a promotion, carrying everything a confirmation dialog or
/// follow-up step needs: the source id (its file is
/// byte-identical after promotion), the target kind, and the drafted concept
/// exactly as written to the target namespace.
#[derive(Debug, Clone)]
pub struct Promotion {
    /// The promoted memory concept's id, e.g. `memory/gotchas`.
    pub source_id: ConceptId,
    /// Which target namespace the draft was written to.
    pub target: PromotionTarget,
    /// The concept as written — present this for review before distribution.
    pub drafted: Concept,
}

impl LocalArgosy {
    /// Opens the bundle rooted at `path` as the local (writable) argosy.
    /// Same hard-failure semantics as [`Argosy::open`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self(Argosy::open(path)?))
    }

    /// Creates a new, empty argosy at `path` — the root `argosy.md` manifest
    /// (version `0.1.0`) plus the four reserved namespace directories — and
    /// opens it. When `name` is `None`, the directory's basename is used.
    /// Fails when a manifest already exists, when the name cannot be derived,
    /// or when it falls outside the URI charset `[A-Za-z0-9._-]`.
    pub fn init(
        path: impl AsRef<Path>,
        name: Option<&str>,
        description: Option<&str>,
    ) -> Result<Self> {
        let root = path.as_ref();
        let owned_name;
        let name = match name {
            Some(name) => name,
            None => {
                let resolved = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
                owned_name = resolved
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
                    .with_context(|| ValidationSnafu {
                        reason: format!(
                            "cannot derive a bundle name from `{}` (no final directory component); pass an explicit name",
                            root.display()
                        ),
                    })?;
                &owned_name
            }
        };
        ensure!(
            crate::bundle::is_safe_bundle_name(name),
            ValidationSnafu {
                reason: format!(
                    "invalid bundle name `{name}`: only [A-Za-z0-9._-] are allowed (the name appears in `argosy://` URIs)"
                )
            }
        );
        ensure!(
            !root.join("argosy.md").exists(),
            ValidationSnafu {
                reason: format!(
                    "`{}` already contains an argosy (an `argosy.md` manifest exists)",
                    root.display()
                )
            }
        );
        for namespace in Namespace::RESERVED {
            fs::create_dir_all(root.join(namespace)).context(IoSnafu {
                path: root.join(namespace),
            })?;
        }
        let mut frontmatter = Mapping::new();
        let mut field = |key: &str, value: &str| {
            frontmatter.insert(
                Value::String(key.to_string()),
                Value::String(value.to_string()),
            );
        };
        field("type", Manifest::TYPE);
        field("name", name);
        field("argosy_version", "0.1.0");
        if let Some(description) = description {
            field("description", description);
        }
        let concept = Concept::new(frontmatter, format!("# {name}\n"))?;
        concept.to_file(root.join("argosy.md"))?;
        Self::open(root)
    }

    /// Resolves `id` against `namespace`, returning the bundle-relative and
    /// absolute target paths. Refuses custom namespaces, ids outside the
    /// namespace, and reserved filenames; [`ConceptId`]'s parser already
    /// rejects `..`, `\`, and `:`, so a resolved path cannot escape the root.
    fn resolve_path(&self, namespace: &Namespace, id: &ConceptId) -> Result<(PathBuf, PathBuf)> {
        if let Namespace::Custom(name) = namespace {
            return ValidationSnafu {
                reason: format!(
                    "custom namespace `{name}` is producer-owned: the library does not know \
                     its concept contract, so it refuses to write into it"
                ),
            }
            .fail();
        }
        let rel = id.to_relative_path();
        let mut first = rel.components();
        let under_namespace = matches!(
            first.next(),
            Some(Component::Normal(c)) if c == std::ffi::OsStr::new(namespace.as_dir_name())
        ) && first.next().is_some();
        ensure!(
            under_namespace,
            ValidationSnafu {
                reason: format!(
                    "concept id `{id}` is not under the `{}/` namespace",
                    namespace.as_dir_name()
                )
            }
        );
        let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
        ensure!(
            !Namespace::RESERVED_FILENAMES.contains(&name),
            ReservedFilenameSnafu
        );
        let path = self.0.root().join(&rel);
        // Defensive: verify the joined path still lies under the namespace
        // directory. Unreachable via ConceptId's parser, but checked
        // unconditionally in release builds too — never trust a joined path.
        ensure!(
            path.starts_with(self.0.root().join(namespace.as_dir_name())),
            ValidationSnafu {
                reason: format!(
                    "concept id `{id}` resolved outside the `{}/` namespace directory",
                    namespace.as_dir_name()
                )
            }
        );
        Ok((rel, path))
    }

    /// Writes `concept` at `id` in `namespace`, creating parent directories
    /// as needed, and returns the absolute path written. Overwriting an
    /// existing concept at `id` is allowed — this is the deliberate edit
    /// path. The concept is validated *before* anything touches disk: the
    /// library never writes a concept [`Argosy::validate`] would flag.
    pub fn write_concept(
        &self,
        namespace: Namespace,
        id: &ConceptId,
        concept: &Concept,
    ) -> Result<PathBuf> {
        let (rel, path) = self.resolve_path(&namespace, id)?;
        if let Some(err) = contract_violation(&namespace, &rel, concept) {
            return Err(err);
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context(IoSnafu {
                path: parent.to_path_buf(),
            })?;
        }
        concept.to_file(&path)?;
        Ok(path)
    }

    /// Deletes the concept at `id` in `namespace`, pruning now-empty parent
    /// directories up to the namespace root. Deleting a directory-form
    /// skill's entry point alone is refused (remove the whole skill
    /// directory instead); file-form entry points delete normally.
    pub fn delete_concept(&self, namespace: Namespace, id: &ConceptId) -> Result<()> {
        let (rel, path) = self.resolve_path(&namespace, id)?;
        if !path.is_file() {
            return ConceptNotFoundSnafu { id: id.clone() }.fail();
        }
        if namespace == Namespace::Skill
            && let Ok(inner) = rel.strip_prefix("skill")
            && inner.components().count() == 2
            && is_skill_entry_point(inner)
        {
            return NamespaceContractViolationSnafu {
                requirement: "SKL-2",
                detail: format!(
                    "deleting `{rel:?}` alone would orphan the directory-form skill; \
                     delete the whole skill directory `skill/{}/` instead",
                    inner.components().next().map_or("", |c| {
                        match c {
                            Component::Normal(s) => s.to_str().unwrap_or(""),
                            _ => "",
                        }
                    })
                ),
            }
            .fail();
        }
        fs::remove_file(&path).context(IoSnafu { path: path.clone() })?;

        // Prune now-empty parents, stopping at the namespace root.
        let ns_root = self.0.root().join(namespace.as_dir_name());
        let mut dir = path.parent();
        while let Some(d) = dir {
            if d == ns_root || !d.starts_with(&ns_root) {
                break;
            }
            if fs::remove_dir(d).is_err() {
                break; // non-empty or unreadable: leave the rest alone
            }
            dir = d.parent();
        }
        Ok(())
    }

    /// Writes a concept under `memory/`; see
    /// [`LocalArgosy::write_concept`]. The MCP layer maps this 1:1.
    pub fn write_memory(&self, id: &ConceptId, concept: &Concept) -> Result<PathBuf> {
        self.write_concept(Namespace::Memory, id, concept)
    }

    /// Deletes a concept under `memory/`; see [`LocalArgosy::delete_concept`].
    pub fn delete_memory(&self, id: &ConceptId) -> Result<()> {
        self.delete_concept(Namespace::Memory, id)
    }

    /// Writes a rule concept under `styleguide/`, enforcing the rule
    /// contract; see [`LocalArgosy::write_concept`].
    pub fn write_rule(&self, id: &ConceptId, concept: &Concept) -> Result<PathBuf> {
        self.write_concept(Namespace::Styleguide, id, concept)
    }

    /// Deletes a rule concept under `styleguide/`.
    pub fn delete_rule(&self, id: &ConceptId) -> Result<()> {
        self.delete_concept(Namespace::Styleguide, id)
    }

    /// Writes a concept under `document/`; see [`LocalArgosy::write_concept`].
    pub fn write_document(&self, id: &ConceptId, concept: &Concept) -> Result<PathBuf> {
        self.write_concept(Namespace::Document, id, concept)
    }

    /// Promotes the `memory/` concept at `source` into a new concept at
    /// `new_id` under the target namespace. The body is copied verbatim;
    /// frontmatter carries over with a `sources` entry appended and, for
    /// styleguide targets, `type: Styleguide Rule` plus a non-empty
    /// `description` (override first). `new_id` must not exist.
    pub fn promote_memory(
        &self,
        source: &ConceptId,
        target: PromotionTarget,
        new_id: &ConceptId,
        description_override: Option<&str>,
    ) -> Result<Promotion> {
        let source_rel = source.to_relative_path();
        let under_memory = matches!(
            source_rel.components().next(),
            Some(Component::Normal(c)) if c == std::ffi::OsStr::new("memory")
        );
        ensure!(
            under_memory,
            ValidationSnafu {
                reason: format!("promotion source `{source}` must name a concept under `memory/`")
            }
        );
        let source_path = self.0.root().join(&source_rel);
        if !source_path.is_file() {
            return ConceptNotFoundSnafu { id: source.clone() }.fail();
        }
        let source_concept = Concept::from_file(&source_path)?;

        let (target_ns, description) = match target {
            PromotionTarget::Document => (
                Namespace::Document,
                description_override
                    .map(str::to_string)
                    .or_else(|| source_concept.description().map(str::to_string)),
            ),
            PromotionTarget::StyleguideRule => {
                let description = description_override
                    .map(str::to_string)
                    .or_else(|| source_concept.description().map(str::to_string));
                ensure!(
                    description.as_deref().is_some_and(|d| !d.trim().is_empty()),
                    NamespaceContractViolationSnafu {
                        requirement: "STG-3",
                        detail: "a promoted styleguide rule needs a `description` (STG-3); \
                                 the memory note has none and no override was given"
                            .to_string(),
                    }
                );
                (Namespace::Styleguide, description)
            }
        };

        // No silent overwrites: a collision is an error anywhere promotion
        // is concerned.
        let (_, new_path) = self.resolve_path(&target_ns, new_id)?;
        ensure!(
            !new_path.exists(),
            ConceptExistsSnafu { id: new_id.clone() }
        );

        let mut frontmatter = source_concept.frontmatter().clone();
        if target == PromotionTarget::StyleguideRule {
            frontmatter.insert(
                Value::String("type".to_string()),
                Value::String(crate::styleguide::TYPE.to_string()),
            );
        }
        if let Some(description) = description {
            frontmatter.insert(
                Value::String("description".to_string()),
                Value::String(description),
            );
        }
        // Provenance: append, never replace — entries the source
        // concept already carries are preserved untouched.
        let provenance = {
            let mut entry = Mapping::new();
            entry.insert(
                Value::String("resource".to_string()),
                Value::String(source_rel.to_string_lossy().replace('\\', "/")),
            );
            Value::Mapping(entry)
        };
        match frontmatter.get_mut("sources") {
            Some(Value::Sequence(sources)) => sources.push(provenance),
            _ => {
                frontmatter.insert(
                    Value::String("sources".to_string()),
                    Value::Sequence(vec![provenance]),
                );
            }
        }

        let drafted = Concept::new(frontmatter, source_concept.body().to_string())?;
        // Full write path: reserved-filename, prefix, and namespace-contract
        // checks all apply to the draft too.
        self.write_concept(target_ns, new_id, &drafted)?;

        Ok(Promotion {
            source_id: source.clone(),
            target,
            drafted,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use tempfile::TempDir;

    use super::*;
    use crate::bundle::Severity;
    use crate::error::Error;
    use crate::testutil::fixture_copy;

    fn id(s: &str) -> ConceptId {
        ConceptId::from_str(s).unwrap()
    }

    fn note_concept() -> Concept {
        Concept::from_str("---\ntype: Session Note\n---\n# Note\n\nContent.\n").unwrap()
    }

    // --- init ---

    #[test]
    fn init_creates_a_conformant_bundle_and_derives_the_name() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("my-bundle");

        let local = LocalArgosy::init(&root, None, Some("What this bundle knows.")).unwrap();

        assert_eq!(local.manifest().name(), "my-bundle");
        assert_eq!(local.manifest().argosy_version().to_string(), "0.1.0");
        for namespace in Namespace::RESERVED {
            assert!(root.join(namespace).is_dir(), "missing {namespace}/");
        }
        assert!(root.join("argosy.md").is_file());
        let report = Argosy::validate(&root);
        assert!(
            report.is_conformant(),
            "a freshly initialized bundle opens and validates: {report}"
        );
    }

    #[test]
    fn init_second_run_fails_and_changes_nothing() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("bundle");
        LocalArgosy::init(&root, None, None).unwrap();
        let manifest_before = fs::read_to_string(root.join("argosy.md")).unwrap();

        let err = LocalArgosy::init(&root, Some("other"), None).unwrap_err();
        assert!(
            err.to_string().contains("already contains an argosy"),
            "unexpected error: {err}"
        );
        assert_eq!(
            fs::read_to_string(root.join("argosy.md")).unwrap(),
            manifest_before
        );
    }

    #[test]
    fn init_rejects_names_outside_the_uri_charset() {
        let tmp = TempDir::new().unwrap();
        for bad in [
            "has spaces",
            "has/slash",
            "ünicode",
            "..",
            "",
            "trailing:colon",
        ] {
            let err = LocalArgosy::init(tmp.path().join("fresh"), Some(bad), None).unwrap_err();
            assert!(
                err.to_string().contains("invalid bundle name"),
                "`{bad}`: unexpected error: {err}"
            );
        }
    }

    #[test]
    fn init_honors_an_explicit_name_over_the_directory_basename() {
        let tmp = TempDir::new().unwrap();
        let local =
            LocalArgosy::init(tmp.path().join("wrong-name"), Some("right-name"), None).unwrap();
        assert_eq!(local.manifest().name(), "right-name");
    }

    fn rule_concept() -> Concept {
        Concept::from_str(
            "---\ntype: Styleguide Rule\ndescription: Never swallow errors.\n---\n# Rule\n\nHandle them.\n",
        )
        .unwrap()
    }

    #[test]
    fn write_memory_round_trips_and_creates_nested_dirs() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let concept = note_concept();
        let path = local
            .write_memory(&id("memory/sessions/2026-08/rate-limit"), &concept)
            .unwrap();
        assert!(path.ends_with("memory/sessions/2026-08/rate-limit.md"));
        let reread = Concept::from_file(&path).unwrap();
        assert_eq!(reread, concept);
        assert!(
            local
                .concepts(&Namespace::Memory)
                .unwrap()
                .iter()
                .any(|(cid, _)| cid.as_str() == "memory/sessions/2026-08/rate-limit")
        );
    }

    #[test]
    fn write_rule_round_trips_in_nested_subdirs() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let concept = rule_concept();
        let path = local
            .write_rule(
                &id("styleguide/python/error-handling/no-bare-except"),
                &concept,
            )
            .unwrap();
        assert_eq!(Concept::from_file(&path).unwrap(), concept);
        let rules = crate::styleguide::StyleguideRule::list(&local).unwrap();
        assert!(
            rules
                .iter()
                .any(|r| r.id().as_str() == "styleguide/python/error-handling/no-bare-except")
        );
    }

    #[test]
    fn write_document_round_trips() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let concept = Concept::from_str(
            "---\ntype: Decision\ndescription: Cache responses.\n---\n# Decision\n\nWe cache.\n",
        )
        .unwrap();
        let path = local
            .write_document(&id("document/decisions/2026-08-caching"), &concept)
            .unwrap();
        assert_eq!(Concept::from_file(&path).unwrap(), concept);
    }

    #[test]
    fn write_refuses_reserved_filename() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        for target in ["memory/index", "memory/log", "memory/argosy"] {
            let err = local
                .write_memory(&id(target), &note_concept())
                .unwrap_err();
            assert!(
                matches!(err, Error::ReservedFilename),
                "{target}: got {err:?}"
            );
            assert!(!tmp.path().join(format!("{target}.md")).exists());
        }
    }

    #[test]
    fn write_refuses_custom_namespace() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let err = local
            .write_concept(
                Namespace::custom("roadmap").unwrap(),
                &id("roadmap/plans-2"),
                &note_concept(),
            )
            .unwrap_err();
        assert!(matches!(err, Error::Validation { .. }), "got {err:?}");
        assert!(err.to_string().contains("producer-owned"));
    }

    #[test]
    fn write_refuses_untyped_concept_with_conformance_id() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let untyped = Concept::from_str("# Just prose\n").unwrap();
        let err = local.write_memory(&id("memory/x"), &untyped).unwrap_err();
        match err {
            Error::NamespaceContractViolation { requirement, .. } => {
                assert_eq!(requirement, "MEM-1")
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn write_rule_without_rule_type_is_stg2() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let err = local
            .write_rule(&id("styleguide/rust/naming/no-rule-type"), &note_concept())
            .unwrap_err();
        match err {
            Error::NamespaceContractViolation { requirement, .. } => {
                assert_eq!(requirement, "STG-2")
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn write_rule_without_description_is_stg3() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let concept =
            Concept::from_str("---\ntype: Styleguide Rule\n---\n# Rule\n\nBody.\n").unwrap();
        let err = local
            .write_rule(&id("styleguide/rust/naming/no-desc"), &concept)
            .unwrap_err();
        match err {
            Error::NamespaceContractViolation { requirement, .. } => {
                assert_eq!(requirement, "STG-3")
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn write_skill_entry_point_without_description_is_skl4() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let concept = Concept::from_str("---\ntype: Skill\n---\n# Deploy\n\nSteps.\n").unwrap();
        let err = local
            .write_concept(Namespace::Skill, &id("skill/deploy-new"), &concept)
            .unwrap_err();
        match err {
            Error::NamespaceContractViolation { requirement, .. } => {
                assert_eq!(requirement, "SKL-4")
            }
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn write_skill_supporting_material_needs_only_okf_conformance() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        // Not an entry-point position (a `references/` material): a typed
        // concept with no description and the "wrong" type writes fine.
        let concept = Concept::from_str("---\ntype: Note\n---\n# Extra\n\nMaterial.\n").unwrap();
        local
            .write_concept(
                Namespace::Skill,
                &id("skill/rotate-api-keys/references/extra"),
                &concept,
            )
            .unwrap();
    }

    #[test]
    fn concept_id_rejects_dotdot() {
        let err = ConceptId::from_str("memory/../secret").unwrap_err();
        assert!(err.to_string().contains(".."), "got {err}");
        let err = Namespace::custom("..").unwrap_err();
        assert!(err.to_string().contains("invalid"), "got {err}");
    }

    #[test]
    fn write_refuses_id_outside_the_named_namespace() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let err = local
            .write_memory(&id("document/escaped"), &note_concept())
            .unwrap_err();
        assert!(matches!(err, Error::Validation { .. }), "got {err:?}");
    }

    #[test]
    fn promote_to_document_copies_and_cites_source() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let source_path = tmp.path().join("memory/gotchas.md");
        let before = fs::read(&source_path).unwrap();

        let promotion = local
            .promote_memory(
                &id("memory/gotchas"),
                PromotionTarget::Document,
                &id("document/rate-limit-retry-gotcha"),
                None,
            )
            .unwrap();

        // The source is byte-identical afterwards.
        assert_eq!(fs::read(&source_path).unwrap(), before);
        // A new, independent concept exists at the target id.
        let drafted = &promotion.drafted;
        let listed = local
            .concepts(&Namespace::Document)
            .unwrap()
            .into_iter()
            .find(|(cid, _)| cid.as_str() == "document/rate-limit-retry-gotcha")
            .unwrap()
            .1;
        assert_eq!(listed, *drafted, "what was written is what was returned");
        assert_eq!(drafted.concept_type(), Some("Session Note"));
        // `sources` cites the bundle-relative memory path.
        let sources = drafted.get("sources").unwrap().as_sequence().unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(
            sources[0].get("resource").unwrap().as_str().unwrap(),
            "memory/gotchas.md"
        );

        // No silent overwrites: promoting again to the same id fails.
        let err = local
            .promote_memory(
                &id("memory/gotchas"),
                PromotionTarget::Document,
                &id("document/rate-limit-retry-gotcha"),
                None,
            )
            .unwrap_err();
        assert!(matches!(err, Error::ConceptExists { .. }), "got {err:?}");
    }

    #[test]
    fn promote_to_styleguide_requires_a_description() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        // `memory/gotchas.md` has no `description`; without an override the
        // promotion must fail rather than write an invalid rule.
        let err = local
            .promote_memory(
                &id("memory/gotchas"),
                PromotionTarget::StyleguideRule,
                &id("styleguide/general/rate-limit-retry"),
                None,
            )
            .unwrap_err();
        match err {
            Error::NamespaceContractViolation { requirement, .. } => {
                assert_eq!(requirement, "STG-3")
            }
            other => panic!("got {other:?}"),
        }
        assert!(!tmp.path().join("styleguide/general").exists());
    }

    #[test]
    fn promote_to_styleguide_uses_override_and_sets_rule_type() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let promotion = local
            .promote_memory(
                &id("memory/gotchas"),
                PromotionTarget::StyleguideRule,
                &id("styleguide/general/rate-limit-retry"),
                Some("Preserve the original timestamp when retrying."),
            )
            .unwrap();
        assert_eq!(
            promotion.drafted.concept_type(),
            Some("Styleguide Rule"),
            "PROM-4/STG-2"
        );
        assert_eq!(
            promotion.drafted.description(),
            Some("Preserve the original timestamp when retrying.")
        );
        let sources = promotion
            .drafted
            .get("sources")
            .unwrap()
            .as_sequence()
            .unwrap();
        assert_eq!(
            sources[0].get("resource").unwrap().as_str().unwrap(),
            "memory/gotchas.md"
        );
        // The written rule is listable like any other.
        let rules = crate::styleguide::StyleguideRule::list(&local).unwrap();
        assert!(
            rules
                .iter()
                .any(|r| r.id().as_str() == "styleguide/general/rate-limit-retry")
        );
    }

    #[test]
    fn promote_preserves_preseeded_sources() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let concept = Concept::from_str(
            "---\ntype: Session Note\ndescription: d\nsources:\n  - resource: document/architecture.md\n---\nBody.\n",
        )
        .unwrap();
        local
            .write_memory(&id("memory/with-sources"), &concept)
            .unwrap();
        let promotion = local
            .promote_memory(
                &id("memory/with-sources"),
                PromotionTarget::Document,
                &id("document/promoted-with-sources"),
                None,
            )
            .unwrap();
        let resources: Vec<_> = promotion
            .drafted
            .get("sources")
            .unwrap()
            .as_sequence()
            .unwrap()
            .iter()
            .map(|s| s.get("resource").unwrap().as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            resources,
            vec!["document/architecture.md", "memory/with-sources.md"]
        );
    }

    #[test]
    fn promote_leaves_source_and_tree_conformant() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        local
            .promote_memory(
                &id("memory/gotchas"),
                PromotionTarget::Document,
                &id("document/rate-limit-retry-gotcha"),
                None,
            )
            .unwrap();
        // The source stays in memory/ (auto-delete never happens).
        assert!(tmp.path().join("memory/gotchas.md").is_file());
        assert!(
            local
                .concepts(&Namespace::Memory)
                .unwrap()
                .iter()
                .any(|(cid, _)| cid.as_str() == "memory/gotchas")
        );
        // The whole tree (with the promoted concept) still validates clean.
        let report = Argosy::validate(tmp.path());
        assert!(
            report.errors().next().is_none(),
            "unexpected errors:\n{report}"
        );
        // Info about memory/ is expected; nothing worse.
        assert!(
            report
                .findings()
                .iter()
                .all(|f| f.severity != Severity::Warning
                    || f.id == Some("§4.2")
                    || f.id == Some("STG-4"))
        );
    }

    #[test]
    fn promote_unknown_source_is_not_found() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let err = local
            .promote_memory(
                &id("memory/nope"),
                PromotionTarget::Document,
                &id("document/x"),
                None,
            )
            .unwrap_err();
        assert!(matches!(err, Error::ConceptNotFound { .. }), "got {err:?}");
    }

    #[test]
    fn promote_refuses_non_memory_source() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let err = local
            .promote_memory(
                &id("document/architecture"),
                PromotionTarget::Document,
                &id("document/x"),
                None,
            )
            .unwrap_err();
        assert!(matches!(err, Error::Validation { .. }), "got {err:?}");
    }

    #[test]
    fn delete_memory_prunes_empty_parents_to_namespace_root() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        local
            .write_memory(&id("memory/tmp/scratch/note"), &note_concept())
            .unwrap();
        local.delete_memory(&id("memory/tmp/scratch/note")).unwrap();
        assert!(!tmp.path().join("memory/tmp").exists());
        assert!(
            tmp.path().join("memory").is_dir(),
            "namespace root survives"
        );
        assert!(tmp.path().join("memory/gotchas.md").is_file());
    }

    #[test]
    fn delete_missing_concept_is_not_found() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let err = local.delete_memory(&id("memory/nope")).unwrap_err();
        assert!(matches!(err, Error::ConceptNotFound { .. }), "got {err:?}");
    }

    #[test]
    fn delete_refuses_directory_form_skill_entry_point() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        let err = local
            .delete_concept(
                Namespace::Skill,
                &id("skill/rotate-api-keys/rotate-api-keys"),
            )
            .unwrap_err();
        match err {
            Error::NamespaceContractViolation {
                requirement,
                detail,
            } => {
                assert_eq!(requirement, "SKL-2");
                assert!(detail.contains("skill/rotate-api-keys/"), "got {detail}");
            }
            other => panic!("got {other:?}"),
        }
        assert!(
            tmp.path()
                .join("skill/rotate-api-keys/rotate-api-keys.md")
                .is_file()
        );
    }

    #[test]
    fn delete_file_form_skill_entry_point_works() {
        let tmp = fixture_copy("valid-acme-billing");
        let local = LocalArgosy::open(tmp.path()).unwrap();
        local
            .delete_concept(Namespace::Skill, &id("skill/reconcile-ledger"))
            .unwrap();
        assert!(!tmp.path().join("skill/reconcile-ledger.md").exists());
        assert!(tmp.path().join("skill").is_dir());
    }
}
