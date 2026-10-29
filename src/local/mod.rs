//! The local (writable) argosy: the one bundle tools may write to.

mod ops;

#[cfg(test)]
mod tests;

use yaml_serde::Value;

use std::ops::Deref;
use std::path::{Component, Path};

use crate::bundle::{Argosy, Namespace, Severity};
use crate::concept::{Concept, ConceptId};
use crate::error::{NamespaceContractViolationSnafu, Result};

/// The requirement ID under which a missing frontmatter `type` is reported when refusing a write.
fn conformance_requirement(namespace: &Namespace) -> &'static str {
    match namespace {
        Namespace::Document => "DOC-1",
        Namespace::Memory => "MEM-1",
        Namespace::Styleguide => "STG-1",
        _ => "OKF concept conformance",
    }
}

/// The `type` auto-filled onto a memory concept whose frontmatter carries no usable one.
///
/// Memory is the session-scratch namespace, so writes fill it in instead of rejecting.
const MEMORY_TYPE: &str = "Memory";

/// Returns `concept` unchanged when it already carries a `type`, else a copy
/// with `type: Memory` inserted (replacing an empty or non-string value).
fn with_memory_type(concept: &Concept) -> Result<Concept> {
    if concept.concept_type().is_some_and(|t| !t.trim().is_empty()) {
        return Ok(concept.clone());
    }
    let mut frontmatter = concept.frontmatter().clone();
    frontmatter.insert(
        Value::String("type".to_string()),
        Value::String(MEMORY_TYPE.to_string()),
    );
    Concept::new(frontmatter, concept.body().to_string())
}

/// True iff `inner` — a path relative to `skill/` — names a skill entry-point position: file
/// form `foo.md` at the top level, or directory form `foo/foo.md`.
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

/// The contract violation that keeps a `concept` out of `namespace` at bundle-relative path
/// `rel`, if any.
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
            // OKF conformance was checked above, so the checks' skip path can't hide a problem.
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

/// The local, writable argosy — the only type with write APIs.
///
/// Derefs to [`Argosy`], so every read API is available unchanged.
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
    /// A `type: Styleguide Rule` concept under `styleguide/`, held to the rule contract.
    StyleguideRule,
}

/// The outcome of a promotion: the source id (whose file stays byte-identical), the target
/// kind, and the drafted concept exactly as written.
#[derive(Debug, Clone)]
pub struct Promotion {
    /// The promoted memory concept's id, e.g. `memory/gotchas`.
    pub source_id: ConceptId,
    /// Which target namespace the draft was written to.
    pub target: PromotionTarget,
    /// The concept as written — present this for review before distribution.
    pub drafted: Concept,
}
