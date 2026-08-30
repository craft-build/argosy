//! The write surface of [`LocalArgosy`]: namespace contracts, writes,
//! deletes, and promotion.

use std::fs;
use std::path::{Component, Path, PathBuf};

use snafu::{OptionExt, ResultExt, ensure};

use crate::bundle::{Argosy, Manifest, Namespace};
use crate::concept::{Concept, ConceptId};
use crate::error::{
    ConceptExistsSnafu, ConceptNotFoundSnafu, IoSnafu, NamespaceContractViolationSnafu,
    ReservedFilenameSnafu, Result, ValidationSnafu,
};
use yaml_serde::{Mapping, Value};

use super::LocalArgosy;
use super::{
    Promotion, PromotionTarget, contract_violation, is_skill_entry_point, with_memory_type,
};

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
        // Memory is the only namespace whose write surface auto-fills the
        // OKF `type` instead of rejecting — every other namespace keeps the
        // strict contract below.
        let concept = if namespace == Namespace::Memory {
            with_memory_type(concept)?
        } else {
            concept.clone()
        };
        if let Some(err) = contract_violation(&namespace, &rel, &concept) {
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
    /// [`LocalArgosy::write_concept`]. The MCP layer maps this 1:1. A
    /// frontmatter `type` that is missing or empty is auto-filled as
    /// `type: Memory` instead of being rejected — MEM-1 still holds on disk.
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

    /// Deletes a concept under `document/`; see
    /// [`LocalArgosy::delete_concept`].
    pub fn delete_document(&self, id: &ConceptId) -> Result<()> {
        self.delete_concept(Namespace::Document, id)
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
