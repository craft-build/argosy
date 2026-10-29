//! The multi-argosy project context: one local (writable) argosy plus
//! any number of imported (read-only) ones, with qualified identity,
//! `argosy://` URIs, and precedence-ordered aggregate listings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use snafu::ensure;

use crate::bundle::{Argosy, Namespace};
use crate::concept::{Concept, ConceptId};
use crate::error::{
    DuplicateArgosyNameSnafu, InvalidUriSnafu, NotAnArgosySnafu, Result, UnknownArgosySnafu,
    ValidationSnafu,
};
use crate::local::LocalArgosy;
use crate::skill::Skill;
use crate::styleguide::StyleguideRule;

/// The characters an `argosy://` URI body may contain (v1 has no percent-encoding).
const URI_CHARSET: &str = "[A-Za-z0-9._-/]";

/// A concept's identity qualified by its argosy: two argosys both defining
/// `document/architecture.md` are two distinct ids.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
pub struct QualifiedConceptId {
    /// The owning argosy's manifest name (e.g. `acme-billing`).
    pub argosy: String,
    /// The namespace the concept lives in.
    pub namespace: Namespace,
    /// The concept's bundle-relative id, including the namespace prefix.
    pub id: ConceptId,
}

impl QualifiedConceptId {
    /// Formats this identity as an `argosy://<argosy>/<namespace>/<concept-id>`
    /// URI.
    ///
    /// The fields are public, so an unrepresentable value will not
    /// round-trip through [`QualifiedConceptId::from_uri`].
    pub fn to_uri(&self) -> String {
        format!("argosy://{}/{}", self.argosy, self.id)
    }

    /// Parses an `argosy://` URI strictly, rejecting malformed, non-canonical,
    /// or out-of-charset URIs as [`crate::error::Error::InvalidUri`].
    pub fn from_uri(uri: &str) -> Result<Self> {
        let bad = |reason: &str| {
            InvalidUriSnafu {
                uri: uri.to_string(),
                reason: reason.to_string(),
            }
            .fail()
        };
        let Some(body) = uri.strip_prefix("argosy://") else {
            return bad("the scheme must be `argosy://`");
        };
        ensure!(
            body.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/')),
            InvalidUriSnafu {
                uri: uri.to_string(),
                reason: format!(
                    "characters outside {URI_CHARSET} are not representable (v1 has no percent-encoding)"
                ),
            }
        );
        let Some((argosy, path)) = body.split_once('/') else {
            return bad("expected `argosy://<argosy>/<namespace>/<concept-id>`");
        };
        if argosy.is_empty() {
            return bad("the argosy name is empty");
        }
        let namespace_segment = path.split('/').next().unwrap_or_default();
        let namespace = Namespace::from_dir_name(namespace_segment);
        if !namespace.is_reserved() {
            return bad(&format!(
                "namespace `{namespace_segment}` is not one of the reserved names {} \
                 (custom namespaces are not addressable by URI: concept identity across \
                 them is producer-defined)",
                Namespace::RESERVED.join(", ")
            ));
        }
        if !path.contains('/') {
            return bad("expected a namespace segment and at least one concept-id segment");
        }
        if path.split('/').any(|s| s == ".") || path.ends_with(".md") {
            return bad(
                "non-canonical spelling: ids are extensionless, so `.` segments and a \
                 trailing `.md` are rejected rather than normalized",
            );
        }
        let id: ConceptId = path
            .parse::<ConceptId>()
            .map_err(|e: crate::error::Error| {
                InvalidUriSnafu {
                    uri: uri.to_string(),
                    reason: e.to_string(),
                }
                .build()
            })?;
        // `path` begins with the namespace segment, so id and namespace agree.
        Ok(QualifiedConceptId {
            argosy: argosy.to_string(),
            namespace,
            id,
        })
    }
}

/// A borrowed reference to one of the context's argosys, distinguishing the
/// local one (write-bearing) from imported ones (read-only).
#[derive(Debug)]
pub enum ArgosyRef<'a> {
    /// The local, writable argosy.
    Local(&'a LocalArgosy),
    /// An imported, read-only argosy.
    Imported(&'a Argosy),
}

/// A skill in an aggregate listing, tagged with its origin argosy and
/// whether a higher-precedence argosy shadows it under the same name.
#[derive(Debug, Clone)]
pub struct SkillListing {
    /// The manifest name of the argosy providing this skill.
    pub argosy: String,
    /// The skill itself.
    pub skill: Skill,
    /// True iff an earlier argosy shadows this skill in `resolve_skill`.
    pub shadowed: bool,
}

/// A styleguide rule in an aggregate listing, tagged with its origin argosy.
#[derive(Debug, Clone)]
pub struct RuleListing {
    /// The manifest name of the argosy providing this rule.
    pub argosy: String,
    /// The rule itself.
    pub rule: StyleguideRule,
}

/// An import refused at open because its integrity sidecar
/// ([`crate::package::INTEGRITY_FILENAME`]) disagreed with the checkout's
/// contents.
///
/// Never served, but recorded rather than swallowed so surfaces can report
/// the excluded content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludedImport {
    /// Best-effort identity of the refused checkout: its directory name
    /// (a state-dir checkout is named after the `argosy pull`).
    pub name: String,
    /// Why it was refused: the `{:#}` chain of the integrity error.
    pub reason: String,
}

/// The active argosys of a project: exactly one local (writable) argosy and
/// any number of imported (read-only) ones.
#[derive(Debug)]
pub struct ProjectContext {
    local: LocalArgosy,
    imported: Vec<Argosy>,
    excluded: Vec<ExcludedImport>,
    /// The git commit each import's checkout resolved to at open (by
    /// manifest name — [`ProjectContext::import_commit`]); checkouts
    /// without git metadata have no entry.
    import_commits: HashMap<String, String>,
}

impl ProjectContext {
    /// Opens the local argosy plus every imported one, in registration order.
    ///
    /// An import whose integrity sidecar disagrees with its contents is
    /// excluded (see [`ProjectContext::excluded_imports`]) so the healthy
    /// argosys still open; duplicate manifest names are an error.
    pub fn open(
        local_path: impl AsRef<Path>,
        imported_paths: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Self> {
        let local = LocalArgosy::open(local_path)?;
        let mut imported = Vec::new();
        let mut excluded = Vec::new();
        let mut import_commits = HashMap::new();
        for path in imported_paths {
            // Books without a stamp (pre-stamp checkouts, hosts pointing
            // at arbitrary trees) keep the standing rule: a present
            // sidecar is verified, an absent one was never packaged (a
            // trust/tier question, see [`crate::trust`]).
            if let Err(reason) = crate::pull::verify_import_integrity(&path) {
                excluded.push(ExcludedImport {
                    name: checkout_label(&path),
                    reason,
                });
                continue;
            }
            let argosy = Argosy::open(&path)?;
            // File-read git provenance; no subprocess per open.
            if let Some(commit) = crate::pull::checkout_head_commit(&path) {
                import_commits.insert(argosy.manifest().name().to_string(), commit);
            }
            imported.push(argosy);
        }

        let local_name = local.manifest().name();
        ensure!(
            !imported.iter().any(|a| a.manifest().name() == local_name),
            DuplicateArgosyNameSnafu {
                name: local_name.to_string()
            }
        );
        for (i, argosy) in imported.iter().enumerate() {
            let name = argosy.manifest().name();
            ensure!(
                !imported[..i].iter().any(|a| a.manifest().name() == name),
                DuplicateArgosyNameSnafu {
                    name: name.to_string()
                }
            );
        }
        Ok(Self {
            local,
            imported,
            excluded,
            import_commits,
        })
    }

    /// The local argosy with its full write surface.
    ///
    /// The only write-bearing accessor: imports are read-only, including
    /// any `memory/` directory they happen to contain.
    pub fn local(&self) -> &LocalArgosy {
        &self.local
    }

    /// Opens the standard argosy set of a project: the local bundle, the
    /// project's pulled checkouts ([`crate::pull::project_argosy_dir`]),
    /// then the global store.
    pub fn open_project(project_root: impl AsRef<Path>) -> Result<Self> {
        let state = crate::pull::state_dir()?;
        Self::open_project_with_state(project_root, &state)
    }

    /// [`Self::open_project`] with an explicit state root, so hosts and
    /// tests inject a tempdir instead of touching `~/.local/state`.
    pub fn open_project_with_state(
        project_root: impl AsRef<Path>,
        state_root: &Path,
    ) -> Result<Self> {
        let project_dir = crate::pull::project_argosy_dir_at(state_root, project_root);
        let mut imported = Vec::new();
        collect_checkouts(
            &project_dir,
            Some(crate::pull::LOCAL_ARGOSY_NAME),
            &mut imported,
        );
        collect_checkouts(&state_root.join("global"), None, &mut imported);

        let local = project_dir.join(crate::pull::LOCAL_ARGOSY_NAME);
        ensure!(
            local.join("argosy.md").is_file(),
            NotAnArgosySnafu {
                path: local.clone(),
                reason: format!(
                    "no local `{}` argosy for this project under {} — run `argosy init` in \
                     the project root (argosys live in the state dir, outside the project tree)",
                    crate::pull::LOCAL_ARGOSY_NAME,
                    project_dir.display()
                )
            }
        );
        Self::open(local, imported)
    }

    /// The imported argosys, read-only, in registration (precedence) order.
    pub fn imported(&self) -> impl Iterator<Item = &Argosy> {
        self.imported.iter()
    }

    /// The git commit an import's checkout resolved to at open, or `None`
    /// for the local book, unknown names, and git-less checkouts.
    pub fn import_commit(&self, name: &str) -> Option<&str> {
        self.import_commits.get(name).map(String::as_str)
    }

    /// The imports refused at open; see [`ExcludedImport`].
    pub fn excluded_imports(&self) -> &[ExcludedImport] {
        &self.excluded
    }

    /// Finds an active argosy by manifest name — the local one first, then
    /// the imported ones in registration order.
    pub fn argosy_named(&self, name: &str) -> Option<ArgosyRef<'_>> {
        if self.local.manifest().name() == name {
            return Some(ArgosyRef::Local(&self.local));
        }
        self.imported
            .iter()
            .find(|a| a.manifest().name() == name)
            .map(ArgosyRef::Imported)
    }

    /// Resolves a qualified id to its concept, reading the file at the id's
    /// bundle-relative path.
    ///
    /// Refuses ids outside their namespace; symlinks are never followed.
    pub fn resolve(&self, qid: &QualifiedConceptId) -> Result<Concept> {
        let first = qid.id.as_str().split('/').next().unwrap_or_default();
        ensure!(
            first == qid.namespace.as_dir_name() && qid.id.as_str().contains('/'),
            ValidationSnafu {
                reason: format!(
                    "concept id `{}` is not under the `{}/` namespace",
                    qid.id,
                    qid.namespace.as_dir_name()
                ),
            }
        );
        let argosy = match self.argosy_named(&qid.argosy) {
            Some(ArgosyRef::Local(local)) => &**local,
            Some(ArgosyRef::Imported(argosy)) => argosy,
            None => {
                return UnknownArgosySnafu {
                    name: qid.argosy.clone(),
                }
                .fail();
            }
        };
        let not_found = || crate::error::ConceptNotFoundSnafu { id: qid.id.clone() }.fail();
        // A symlinked namespace dir counts as absent crate-wide (`Argosy::namespace_dir`).
        if argosy.namespace_dir(&qid.namespace).is_none() {
            return not_found();
        }
        let mut cursor = argosy.root().to_path_buf();
        let mut target = None;
        for component in qid.id.to_relative_path().components() {
            cursor.push(component);
            let Ok(meta) = std::fs::symlink_metadata(&cursor) else {
                return not_found();
            };
            if meta.file_type().is_symlink() {
                return not_found();
            }
            target = Some(meta);
        }
        if !target.is_some_and(|m| m.file_type().is_file()) {
            return not_found();
        }
        Concept::from_file(cursor)
    }

    /// Parses `uri` ([`QualifiedConceptId::from_uri`]) and resolves it
    /// ([`ProjectContext::resolve`]).
    pub fn read_uri(&self, uri: &str) -> Result<Concept> {
        let qid = QualifiedConceptId::from_uri(uri)?;
        self.resolve(&qid)
    }

    /// Every active argosy in precedence order (local first, then imported
    /// in registration order) paired with its manifest name.
    fn in_precedence_order(&self) -> impl Iterator<Item = (&str, &Argosy)> {
        std::iter::once(&*self.local)
            .chain(self.imported.iter())
            .map(|a| (a.manifest().name(), a))
    }

    /// Lists every skill across all active argosys in precedence order,
    /// each tagged with its origin argosy. Name collisions are flagged
    /// [`SkillListing::shadowed`] rather than dropped.
    pub fn list_skills(&self) -> Result<Vec<SkillListing>> {
        let mut listings = Vec::new();
        for (name, argosy) in self.in_precedence_order() {
            for skill in Skill::list(argosy)? {
                listings.push(SkillListing {
                    argosy: name.to_string(),
                    skill,
                    shadowed: false,
                });
            }
        }
        let mut seen = std::collections::HashSet::new();
        for listing in &mut listings {
            if !seen.insert(listing.skill.name.clone()) {
                listing.shadowed = true;
            }
        }
        Ok(listings)
    }

    /// The highest-precedence skill of that name: the first matching entry
    /// of [`ProjectContext::list_skills`]. Returned by value — a cache
    /// would go stale on writes through [`ProjectContext::local`].
    pub fn resolve_skill(&self, name: &str) -> Result<Option<SkillListing>> {
        Ok(self
            .list_skills()?
            .into_iter()
            .find(|listing| listing.skill.name == name))
    }

    /// Lists styleguide rules across all active argosys (combined, never
    /// replaced), narrowed by exact facet matches via [`StyleguideRule::filter`]
    /// (`None` = no constraint).
    ///
    /// These filesystem-level listings are the ground truth the semantic
    /// index must match.
    pub fn list_rules(
        &self,
        language: Option<&str>,
        category: Option<&str>,
    ) -> Result<Vec<RuleListing>> {
        let mut listings = Vec::new();
        for (name, argosy) in self.in_precedence_order() {
            for rule in StyleguideRule::filter(StyleguideRule::list(argosy)?, language, category) {
                listings.push(RuleListing {
                    argosy: name.to_string(),
                    rule,
                });
            }
        }
        Ok(listings)
    }
}

/// Best-effort identity of an excluded import for surfacing: the
/// checkout's directory name, or the whole path when it has no file name.
fn checkout_label(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Appends the bundle checkouts directly under `dir`: every subdirectory
/// holding `argosy.md`, sorted for deterministic precedence, never
/// following symlinks. `skip` excludes one checkout name. A missing or
/// unreadable `dir` means "no argosies here", not an error.
fn collect_checkouts(dir: &Path, skip: Option<&str>, out: &mut Vec<PathBuf>) {
    let Ok(mut entries) = std::fs::read_dir(dir).map(|rd| rd.flatten().collect::<Vec<_>>()) else {
        return;
    };
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        if skip.is_some_and(|skip| name == skip) {
            continue;
        }
        let Ok(ty) = entry.file_type() else { continue };
        if !ty.is_dir() {
            continue;
        }
        let path = entry.path();
        if path.join("argosy.md").is_file() {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests;
